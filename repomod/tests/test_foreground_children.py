"""A command child survives SIGINT delivered to the parent's process group."""

import contextlib
import importlib.util
import os
import signal
import socket
import subprocess
import sys
import threading
from pathlib import Path
from typing import Any

import pytest

from repomod.git import _run
from repomod.lint import run_actionlint
from repomod.main import preflight
from repomod.sync import graphql, search_page
from repomod.utils import ShutdownError, interrupt


def _posix_int(module: object, name: str) -> int:
    value = getattr(module, name, None)
    if not isinstance(value, int):
        raise OSError(f"{name} is required")
    return value


def _posix_call(module: object, name: str) -> Any:
    value = getattr(module, name, None)
    if not callable(value):
        raise OSError(f"{name} is required")
    return value


_AF_UNIX = _posix_int(socket, "AF_UNIX")
_SOCK_STREAM = _posix_int(socket, "SOCK_STREAM")
_getpgrp = _posix_call(os, "getpgrp")
_getsid = _posix_call(os, "getsid")
_setpgid = _posix_call(os, "setpgid")
_killpg = _posix_call(os, "killpg")

_FIX_PATH = (
    Path(__file__).resolve().parent.parent
    / "src"
    / "repomod"
    / "fixes"
    / "pin-gha"
    / "main.py"
)
_spec = importlib.util.spec_from_file_location("pin_gha_foreground", _FIX_PATH)
assert _spec is not None
assert _spec.loader is not None
pin_gha = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(pin_gha)

# Identity, then block on stdin. On SIGINT, record that fact and die with the
# default action so the parent's wait sees a signal death, not a traceback.
_STANDIN = """
import os
import signal
import socket
import sys

_sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
_sock.connect(os.environ["FOREGROUND_CHILD_SOCKET"])
_fd = _sock.fileno()


def _on_sigint(signum, frame):
    signal.signal(signal.SIGINT, signal.SIG_DFL)
    os.write(_fd, b"signaled\\n")
    os.kill(os.getpid(), signal.SIGINT)


signal.signal(signal.SIGINT, _on_sigint)
_sock.sendall(f"{os.getsid(0)} {os.getpid()} {os.getpgrp()}\\n".encode())
while os.read(0, 4096):
    pass
os.write(_fd, b"survived\\n")
_sock.close()
"""

_SITES = (
    "run_actionlint",
    "preflight",
    "graphql",
    "search_page",
    "resolve_ref",
    "git._run",
)

_CHILD_EXITS = (
    SystemExit,
    subprocess.CalledProcessError,
    subprocess.TimeoutExpired,
    ShutdownError,
    ValueError,
    OSError,
)


def _invoke(site: str, repo: Path) -> None:
    if site == "run_actionlint":
        run_actionlint(repo, [".github/workflows/ci.yml"])
    elif site == "preflight":
        preflight()
    elif site == "graphql":
        graphql("query { viewer { login } }")
    elif site == "search_page":
        search_page(1)
    elif site == "resolve_ref":
        pin_gha.resolve_ref("actions/checkout", "v4")
    elif site == "git._run":
        _run(["git", "status"], capture_output=True, text=True)
    else:
        raise AssertionError(site)


def _readline(conn: socket.socket) -> bytes:
    buf = bytearray()
    while b"\n" not in buf:
        chunk = conn.recv(256)
        if not chunk:
            break
        buf += chunk
    return bytes(buf)


def _read_rest(conn: socket.socket) -> bytes:
    buf = bytearray()
    while True:
        chunk = conn.recv(256)
        if not chunk:
            break
        buf += chunk
    return bytes(buf)


def _close_fd(box: list[int | None]) -> None:
    fd = box[0]
    if fd is None:
        return
    box[0] = None
    os.close(fd)


def _drive(site: str, repo: Path, bin_dir: Path, sock_path: Path) -> dict[str, object]:
    script = bin_dir / "stub"
    script.write_text(f"#!{sys.executable}\n{_STANDIN}")
    script.chmod(0o755)
    for name in ("actionlint", "gh", "git"):
        os.symlink(script, bin_dir / name)

    if sock_path.exists():
        sock_path.unlink()
    server = socket.socket(_AF_UNIX, _SOCK_STREAM)
    server.bind(str(sock_path))
    server.listen(1)
    read_fd, write_fd = os.pipe()
    write_box: list[int | None] = [write_fd]
    outcome: dict[str, object] = {}
    previous = signal.getsignal(signal.SIGINT)
    saved_stdin = os.dup(0)
    old_pgid = _getpgrp()
    # Own process group: SIGINT must not reach the shell that launched us.
    _setpgid(0, 0)
    if _getpgrp() != os.getpid():
        raise RuntimeError("refusing to signal a process group we do not lead")

    def observe() -> None:
        conn, _addr = server.accept()
        try:
            line = _readline(conn)
            sid_s, _pid_s, pgid_s = line.split()
            outcome["sid"] = int(sid_s)
            outcome["pgid"] = int(pgid_s)
            _killpg(_getpgrp(), signal.SIGINT)
            # Block until the parent handler runs. No timer: retry must see
            # the interrupt and not sleep between attempts.
            interrupt.event.wait()
            if int(pgid_s) == _getpgrp():
                # Leave the pipe open so a same-group child cannot exit on
                # EOF before the signal is delivered.
                outcome["tail"] = _read_rest(conn)
            else:
                _close_fd(write_box)
                outcome["tail"] = _read_rest(conn)
        finally:
            conn.close()

    observer = threading.Thread(target=observe)
    observer.start()
    os.dup2(read_fd, 0)
    os.close(read_fd)
    interrupt.event.clear()
    interrupt.install()
    caught: BaseException | None = None
    try:
        try:
            _invoke(site, repo)
        except _CHILD_EXITS as exc:
            caught = exc
    finally:
        _close_fd(write_box)
        server.close()
        interrupt.event.set()
        observer.join()
        os.dup2(saved_stdin, 0)
        os.close(saved_stdin)
        signal.signal(signal.SIGINT, previous)
        interrupt.event.clear()
        with contextlib.suppress(OSError):
            _setpgid(0, old_pgid)
        if sock_path.exists():
            sock_path.unlink()
    outcome["caught"] = caught
    return outcome


@pytest.mark.parametrize("site", _SITES)
def test_child_is_outside_the_parent_session_and_survives_sigint(
    site: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = tmp_path / "acme-backend"
    repo.mkdir()
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    root = Path(os.environ.get("TMPDIR", "/Users/josh/dev/repomod-joshuarli/.work/tmp"))
    root.mkdir(parents=True, exist_ok=True)
    sock_path = root / f"fg{os.getpid()}-{site.replace('.', '_')}.sock"
    assert len(str(sock_path)) < 104
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
    monkeypatch.setenv("FOREGROUND_CHILD_SOCKET", str(sock_path))

    parent_sid = _getsid(0)
    outcome = _drive(site, repo, bin_dir, sock_path)
    parent_pgid_during = outcome.get("pgid")
    tail = outcome.get("tail")
    assert isinstance(tail, bytes), f"{site} child never reported its session"
    child_sid = outcome["sid"]
    child_pgid = outcome["pgid"]
    assert isinstance(child_sid, int)
    assert isinstance(child_pgid, int)
    signaled = b"signaled" in tail
    survived = b"survived" in tail
    problems: list[str] = []
    if child_sid == parent_sid or child_pgid == _getpgrp() or not isinstance(
        parent_pgid_during, int
    ):
        problems.append(
            f"shared the parent session {parent_sid} "
            f"(child_sid={child_sid} child_pgid={child_pgid})"
        )
    if signaled or not survived:
        caught = outcome["caught"]
        returncode = ""
        if isinstance(caught, subprocess.CalledProcessError):
            returncode = f" returncode={caught.returncode}"
        problems.append(
            f"died from SIGINT to the parent process group{returncode}"
        )
    assert not problems, f"{site} child " + " and ".join(problems)
