import fcntl
import hashlib
import os
import time
from contextlib import contextmanager
from pathlib import Path

from .errors import ConflictError, OperationError

CHUNK = 65536
_counter = iter(range(1, 2**62))


def unique_suffix():
    return f"{os.getpid()}.{time.time_ns()}.{next(_counter)}"


def list_dir(path):
    with os.scandir(path) as listing:
        return list(listing)


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def sync_tree(path):
    sync_dir(path)
    for root, _dirs, files in os.walk(path):
        if files:
            sync_dir(root)


def write_file_durable(path, data):
    path = Path(path)
    parent = path.parent
    parent.mkdir(parents=True, exist_ok=True)
    tmp = parent / f".tmp.{os.getpid()}.{time.time_ns()}.{path.name}"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o666)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise
    sync_dir(parent)


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(CHUNK), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _acquire(fd, wait, conflict_message, path):
    try:
        if wait is None:
            fcntl.flock(fd, fcntl.LOCK_EX)
            return
        deadline = time.monotonic() + wait
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise ConflictError(conflict_message or f"another process holds {path}") from None
                time.sleep(0.05)
    except OSError as error:
        raise OperationError(f"lock failed: {error}") from error


@contextmanager
def exclusive_lock(path, wait=None, conflict_message=None):
    fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        _acquire(fd, wait, conflict_message, path)
    except BaseException:
        os.close(fd)
        raise
    try:
        yield
    finally:
        os.close(fd)
