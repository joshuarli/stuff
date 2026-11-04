import hashlib
import os
import struct
import time
from dataclasses import dataclass
from pathlib import Path

from . import wirejson
from .errors import InputError, LimitError, OperationError
from .model import PublishBody

MAX_METADATA_BYTES = 10 * 1024 * 1024
FRAMING_SLACK = 10 * 1024 * 1024
READ_CHUNK = 65536
_counter = iter(range(1, 2**62))


@dataclass
class StagedUpload:
    body: PublishBody
    path: Path
    crate_len: int
    crate_sha256: str

    def discard(self):
        self.path.unlink(missing_ok=True)


def _read_exact(reader, count, what):
    data = bytearray()
    while len(data) < count:
        chunk = reader.read(count - len(data))
        if not chunk:
            raise InputError(f"failed to read {what}: connection closed early")
        data.extend(chunk)
    return bytes(data)


def decode_publish_payload(reader, content_length, max_compressed, staging_dir):
    if content_length < 8:
        raise InputError("upload payload too short")
    if content_length > max_compressed + FRAMING_SLACK:
        raise LimitError("upload payload exceeds size limit")
    staging_dir = Path(staging_dir)
    staging_dir.mkdir(parents=True, exist_ok=True)

    (json_len,) = struct.unpack("<I", _read_exact(reader, 4, "json length"))
    if json_len == 0:
        raise InputError("empty json metadata")
    if json_len > MAX_METADATA_BYTES or json_len > content_length - 8:
        raise LimitError("json metadata length invalid or too large")
    try:
        body = PublishBody.from_obj(wirejson.loads_bytes(_read_exact(reader, json_len, "json body")))
    except InputError as error:
        raise InputError(f"invalid publish json: {error.message}") from error

    (crate_len,) = struct.unpack("<I", _read_exact(reader, 4, "crate length"))
    if crate_len == 0:
        raise InputError("empty crate archive")
    if crate_len > max_compressed:
        raise LimitError("crate archive exceeds max compressed size")
    if 4 + json_len + 4 + crate_len != content_length:
        raise InputError("crate length does not match content length framing")

    spill = staging_dir / f".upload.{os.getpid()}.{time.time_ns()}.{next(_counter)}"
    digest = hashlib.sha256()
    fd = os.open(spill, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "wb") as staged:
            remaining = crate_len
            while remaining > 0:
                chunk = reader.read(min(remaining, READ_CHUNK))
                if not chunk:
                    raise InputError("failed to read crate bytes: connection closed early")
                digest.update(chunk)
                try:
                    staged.write(chunk)
                except OSError as error:
                    raise OperationError(f"failed to stage upload: {error}") from error
                remaining -= len(chunk)
            staged.flush()
            os.fsync(staged.fileno())
    except BaseException:
        spill.unlink(missing_ok=True)
        raise
    return StagedUpload(body, spill, crate_len, digest.hexdigest())
