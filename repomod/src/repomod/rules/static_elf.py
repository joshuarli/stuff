from __future__ import annotations

SCRIPT = r'''#!/usr/bin/env python3
import argparse
import os
import struct
import sys
from pathlib import Path

MACHINES = {62: "x86_64", 183: "aarch64"}
ELFCLASS64 = 2
ELFDATA2LSB = 1
PT_DYNAMIC = 2
PT_INTERP = 3
DT_NULL = 0
DT_NEEDED = 1
TRACE_REQUIRED = ("/usr/lib/libc.a", "crtbegin.o")
TRACE_FORBIDDEN = (
    ".rustup/",
    "/rustlib/",
    "/self-contained/",
    "x86_64-linux-gnu",
    "aarch64-linux-gnu",
    "libc.so",
)


def fail(message):
    print(f"verify-static-elf: {message}", file=sys.stderr)
    return 1


def read_elf(path):
    data = path.read_bytes()
    if len(data) < 64 or data[:4] != b"\x7fELF":
        return None
    if data[4] != ELFCLASS64 or data[5] != ELFDATA2LSB:
        raise ValueError("not a little-endian ELF64 file")
    machine = struct.unpack_from("<H", data, 18)[0]
    if machine not in MACHINES:
        raise ValueError(f"unsupported ELF machine {machine}")
    program_offset = struct.unpack_from("<Q", data, 32)[0]
    entry_size = struct.unpack_from("<H", data, 54)[0]
    entries = struct.unpack_from("<H", data, 56)[0]
    headers = []
    if entries:
        if entry_size < 56:
            raise ValueError("program header table is truncated")
        for index in range(entries):
            offset = program_offset + index * entry_size
            if offset + 56 > len(data):
                raise ValueError("program header table is truncated")
            kind = struct.unpack_from("<I", data, offset)[0]
            file_offset = struct.unpack_from("<Q", data, offset + 8)[0]
            file_size = struct.unpack_from("<Q", data, offset + 32)[0]
            headers.append((kind, file_offset, file_size))
    return machine, data, headers


def dynamic_needed(data, offset, size):
    end = min(len(data), offset + size)
    while offset + 16 <= end:
        tag, value = struct.unpack_from("<qQ", data, offset)
        if tag == DT_NULL:
            return False
        if tag == DT_NEEDED:
            return True
        offset += 16
    return False


def check_elf(path):
    parsed = read_elf(path)
    if parsed is None:
        raise ValueError("not an ELF file")
    machine, data, headers = parsed
    for kind, offset, size in headers:
        if kind == PT_INTERP:
            raise ValueError("has a dynamic ELF interpreter")
        if kind == PT_DYNAMIC and dynamic_needed(data, offset, size):
            raise ValueError("has a dynamic library dependency")
    return machine


def trace_failure(trace):
    try:
        source = trace.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        return f"cannot read linker trace {trace}: {error}"
    for marker in TRACE_FORBIDDEN:
        if marker in source:
            return f"linker trace contains a non-builder path: {marker}"
    for marker in TRACE_REQUIRED:
        if marker not in source:
            return f"linker trace does not name {marker}"
    return None


def main():
    parser = argparse.ArgumentParser(prog="verify-static-elf", allow_abbrev=False)
    parser.add_argument("--trace", type=Path)
    parser.add_argument("artifacts", nargs="+", type=Path)
    args = parser.parse_args()
    explicit = []
    scanned = []
    for artifact in args.artifacts:
        if artifact.is_symlink():
            return fail(f"symlink is not followed: {artifact}")
        if not artifact.exists():
            return fail(f"artifact does not exist: {artifact}")
        if artifact.is_dir():
            for directory, names, filenames in os.walk(artifact, followlinks=False):
                current = Path(directory)
                names[:] = [name for name in names if not (current / name).is_symlink()]
                for name in filenames:
                    path = current / name
                    if path.is_file() and not path.is_symlink():
                        scanned.append(path)
        else:
            explicit.append(artifact)
    machines = set()
    verified = 0
    for path in explicit:
        try:
            machines.add(check_elf(path))
            verified += 1
        except (OSError, ValueError, struct.error) as error:
            return fail(f"{path}: {error}")
    for path in scanned:
        try:
            machine = check_elf(path)
        except ValueError:
            continue
        except (OSError, struct.error) as error:
            return fail(f"{path}: {error}")
        machines.add(machine)
        verified += 1
    if any(artifact.is_dir() for artifact in args.artifacts):
        missing = sorted(set(MACHINES.values()) - set(MACHINES[machine] for machine in machines))
        if missing:
            return fail("expected at least one artifact for " + ", ".join(missing))
    if not machines:
        return fail("no static ELF artifacts found")
    if args.trace is not None:
        error = trace_failure(args.trace)
        if error is not None:
            return fail(error)
    print(f"verified {verified} ELF artifacts")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
'''

SCRIPT_BODY = SCRIPT.split("\n", 1)[1]
STATIC_SMOKE_NAME = "Verify Linux ELF artifacts are fully static"
SCRIPT_MARKER = "ELFCLASS64"


def render_script() -> str:
    return SCRIPT


def render_workflow_step(indentation: int, image_mode: bool) -> list[str]:
    prefix = " " * indentation
    child = prefix + "  "
    script = child + "  "
    invocation = f"{script}python3 -"
    if image_mode:
        invocation += ' --trace "$RUNNER_TEMP/linker-trace"'
    invocation += " dist <<'PY'"
    lines = [f"{prefix}- name: {STATIC_SMOKE_NAME}", f"{child}run: |", f"{script}set -euo pipefail", invocation]
    lines.extend(f"{script}{line}" if line else "" for line in SCRIPT_BODY.splitlines())
    lines.append(f"{script}PY")
    return lines
