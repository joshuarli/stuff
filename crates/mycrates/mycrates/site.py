import os
import shutil
import stat
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

from .errors import InputError, IntegrityError, OperationError
from .fsutil import CHUNK, list_dir, exclusive_lock, sha256_file, sync_dir, sync_tree, unique_suffix

MAX_GENERATION_ID_LENGTH = 128
TEMP_PREFIXES = (".staging.", ".current.tmp.", ".quarantine.")


def is_generation_id(text):
    return (
        0 < len(text) <= MAX_GENERATION_ID_LENGTH
        and text.isascii()
        and all(c.isalnum() or c in "-_" for c in text)
    )


def _validate_generation_id(generation_id):
    if not is_generation_id(generation_id):
        raise InputError(f"unsafe generation id: {generation_id!r}")


@dataclass(frozen=True)
class Activation:
    generation_id: str
    is_noop: bool


def sync_immutable_file(src, dst, expected_hex):
    src, dst = Path(src), Path(dst)
    try:
        src_stat = os.lstat(src)
    except OSError:
        raise IntegrityError(f"missing blob {src}") from None
    if not _is_regular(src_stat):
        raise IntegrityError(f"managed path is not a regular file: {src}")
    try:
        dst_stat = os.lstat(dst)
    except FileNotFoundError:
        dst_stat = None
    except OSError as error:
        raise OperationError(f"could not inspect served blob {dst}: {error}") from error
    if dst_stat is not None:
        if not _is_regular(dst_stat):
            raise IntegrityError(
                f"served blob mismatch for {expected_hex}: destination is not a regular file ({dst})"
            )
        if (dst_stat.st_dev, dst_stat.st_ino) == (src_stat.st_dev, src_stat.st_ino):
            return
        if dst_stat.st_size == src_stat.st_size:
            return
        raise IntegrityError(f"served blob mismatch for {expected_hex}: destination is stale or replaced ({dst})")

    actual = sha256_file(src)
    if actual != expected_hex:
        raise IntegrityError(f"tampered blob {src}: expected {expected_hex}, got {actual}")
    dst.parent.mkdir(parents=True, exist_ok=True)
    try:
        os.link(src, dst)
    except OSError:
        _copy_into_place(src, dst, expected_hex)
    else:
        if sha256_file(dst) != expected_hex:
            raise IntegrityError(f"served blob mismatch for {expected_hex}: destination is stale or replaced ({dst})")
    sync_dir(dst.parent)


def _is_regular(stat_result):
    return stat.S_ISREG(stat_result.st_mode)


def _copy_into_place(src, dst, expected_hex):
    tmp = dst.parent / f".tmp.install.{unique_suffix()}"
    try:
        with open(src, "rb") as source:
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o666)
            with os.fdopen(fd, "wb") as target:
                shutil.copyfileobj(source, target, CHUNK)
                target.flush()
                os.fsync(target.fileno())
        os.replace(tmp, dst)
    except OSError:
        tmp.unlink(missing_ok=True)
        raise IntegrityError(f"served blob changed during install: {expected_hex} ({dst})") from None
    if sha256_file(dst) != expected_hex:
        raise IntegrityError(f"served blob changed during install: {expected_hex} ({dst})")


class Site:
    def __init__(self, site_dir):
        self.site_dir = Path(site_dir)

    @property
    def generations_dir(self):
        return self.site_dir / "generations"

    @property
    def current_link(self):
        return self.site_dir / "current"

    def current_generation(self):
        link = self.current_link
        try:
            stat_result = os.lstat(link)
        except FileNotFoundError:
            return None
        except OSError as error:
            raise OperationError(f"could not inspect current link {link}: {error}") from error
        if not stat.S_ISLNK(stat_result.st_mode):
            raise IntegrityError(f"current link is not a symlink: {link}")
        target = os.readlink(link)
        parts = Path(target).parts
        if len(parts) == 2 and parts[0] == "generations" and parts[1] not in (".", ".."):
            if not is_generation_id(parts[1]):
                raise IntegrityError(f"current link has an unsafe generation target: {target}")
            return parts[1]
        raise IntegrityError(f"current link points outside generations: {target}")

    @contextmanager
    def lock(self, wait):
        path = self.site_dir / ".publish.lock"
        with exclusive_lock(path, wait, f"another publisher holds the publication lock at {path}"):
            yield

    def check_no_symlinks_in_path(self):
        if self.site_dir.is_symlink():
            raise IntegrityError(f"site dir itself cannot be a symlink: {self.site_dir}")
        if self.site_dir.exists():
            for entry in list_dir(self.site_dir):
                if entry.name != "current" and entry.is_symlink():
                    raise IntegrityError(f"unexpected symlink in site dir: {entry.path}")

    def recover_staging(self):
        removed = 0
        if self.site_dir.exists():
            for entry in list_dir(self.site_dir):
                if entry.name.startswith(TEMP_PREFIXES):
                    if entry.is_symlink() or not entry.is_dir(follow_symlinks=False):
                        Path(entry.path).unlink(missing_ok=True)
                    else:
                        shutil.rmtree(entry.path, ignore_errors=True)
                    removed += 1
        return removed

    def ensure_layout(self):
        self.site_dir.mkdir(parents=True, exist_ok=True)
        sync_dir(self.site_dir)
        self.generations_dir.mkdir(parents=True, exist_ok=True)
        sync_dir(self.generations_dir)

    def stage(self):
        path = self.site_dir / f".staging.{unique_suffix()}"
        path.mkdir(parents=True)
        return path

    def publish(self, generation_id, build, verify):
        _validate_generation_id(generation_id)
        self.ensure_layout()
        target = self.generations_dir / generation_id
        if self.current_generation() == generation_id and verify(target):
            return Activation(generation_id, True)
        try:
            target_stat = os.lstat(target)
        except FileNotFoundError:
            target_stat = None
        except OSError as error:
            raise OperationError(f"could not inspect generation {generation_id}: {error}") from error
        if target_stat is not None:
            if not stat.S_ISDIR(target_stat.st_mode):
                raise IntegrityError(f"generation path is not a real directory: {target}")
            if not verify(target):
                raise _immutable_generation_error(generation_id)
            self._switch_current(generation_id)
            return Activation(generation_id, False)
        staging = self.stage()
        try:
            build(staging)
            sync_tree(staging)
        except BaseException:
            shutil.rmtree(staging, ignore_errors=True)
            raise
        return self.activate(staging, generation_id, verify)

    def activate(self, staging, generation_id, verify):
        staging = Path(staging)
        _validate_generation_id(generation_id)
        if staging.parent != self.site_dir:
            raise InputError(f"staging dir is not directly inside site dir: {staging}")
        if not staging.name.startswith(".staging."):
            raise InputError(f"staging dir is not a site staging directory: {staging}")
        target = self.generations_dir / generation_id

        if self.current_generation() == generation_id and verify(target):
            shutil.rmtree(staging, ignore_errors=True)
            sync_dir(self.site_dir)
            return Activation(generation_id, True)

        try:
            target_stat = os.lstat(target)
        except FileNotFoundError:
            target_stat = None
        except OSError as error:
            raise OperationError(f"could not inspect generation {generation_id}: {error}") from error
        if target_stat is None:
            os.rename(staging, target)
            sync_dir(self.generations_dir)
            try:
                verified = verify(target)
            except Exception:
                verified = False
            if not verified:
                quarantined = self._quarantine(generation_id)
                raise IntegrityError(
                    f"installed generation {generation_id} failed verification; moved it to {quarantined}"
                )
        elif not stat.S_ISDIR(target_stat.st_mode):
            shutil.rmtree(staging, ignore_errors=True)
            raise IntegrityError(f"generation path is not a real directory: {target}")
        else:
            if not verify(target):
                shutil.rmtree(staging, ignore_errors=True)
                raise _immutable_generation_error(generation_id)
            shutil.rmtree(staging, ignore_errors=True)
        sync_dir(self.site_dir)
        self._switch_current(generation_id)
        return Activation(generation_id, False)

    def _quarantine(self, generation_id):
        path = self.site_dir / f".quarantine.{generation_id}.{unique_suffix()}"
        os.rename(self.generations_dir / generation_id, path)
        sync_dir(self.generations_dir)
        sync_dir(self.site_dir)
        return path

    def _switch_current(self, generation_id):
        current = self.current_link
        try:
            previous = os.readlink(current)
        except FileNotFoundError:
            previous = None
        except OSError as error:
            raise OperationError(f"could not read current link {current}: {error}") from error
        relative = f"generations/{generation_id}"
        if previous == relative:
            return
        tmp = self.site_dir / f".current.tmp.{unique_suffix()}"
        os.symlink(relative, tmp)
        try:
            os.replace(tmp, current)
        except OSError as error:
            tmp.unlink(missing_ok=True)
            raise OperationError(f"could not switch current to generation {generation_id}: {error}") from error
        try:
            sync_dir(self.site_dir)
        except OSError as error:
            try:
                self._restore_current(previous)
            except OSError:
                raise OperationError(
                    f"current switched to generation {generation_id} but durability failed "
                    f"and rollback was incomplete: {error}"
                ) from error
            raise OperationError(f"could not durably switch current to generation {generation_id}: {error}") from error

    def _restore_current(self, previous):
        if previous is None:
            os.unlink(self.current_link)
        else:
            tmp = self.site_dir / f".current.tmp.{unique_suffix()}"
            os.symlink(previous, tmp)
            try:
                os.replace(tmp, self.current_link)
            except OSError:
                tmp.unlink(missing_ok=True)
                raise
        sync_dir(self.site_dir)


def _immutable_generation_error(generation_id):
    return IntegrityError(
        f"generation {generation_id} already exists with different content; "
        "refusing to replace an immutable generation"
    )

