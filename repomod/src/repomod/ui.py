"""Terminal UI helpers: colors, spinner, progress display, diff highlighting."""

import os
import re
import sys
import threading
from collections.abc import Generator
from contextlib import contextmanager

_COLOR = sys.stdout.isatty()
_EL = "\033[2K"  # erase entire line

# ANSI codes — empty strings when not a TTY, so they can be used directly in f-strings.
RST = "\033[0m" if _COLOR else ""
CYAN = "\033[36m" if _COLOR else ""
GREEN = "\033[32m" if _COLOR else ""
RED = "\033[31m" if _COLOR else ""
DIM = "\033[2m" if _COLOR else ""
BOLD = "\033[1m" if _COLOR else ""
YELLOW = "\033[33m" if _COLOR else ""

_FRAMES = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
_ANSI_RE = re.compile(r"\033\[[0-9;]*m")


def strip_ansi(s: str) -> str:
    return _ANSI_RE.sub("", s)


def visible_len(s: str) -> int:
    return len(strip_ansi(s))


def term_width() -> int:
    try:
        return os.get_terminal_size().columns
    except OSError:
        return 80


def wrap_colored_line(line: str, width: int) -> list[str]:
    """Wrap a line with ANSI codes to fit within `width` visible chars."""
    if visible_len(line) <= width:
        return [line]

    chunks: list[str] = []
    current: list[str] = []
    vis = 0
    i = 0
    while i < len(line):
        m = _ANSI_RE.match(line, i)
        if m:
            current.append(m.group())
            i = m.end()
            continue
        if vis >= width:
            chunks.append("".join(current))
            current = []
            vis = 0
        current.append(line[i])
        vis += 1
        i += 1
    if current:
        chunks.append("".join(current))
    return chunks or [line]


def format_diff(diff: str) -> str:
    """Syntax-highlight a unified diff."""
    if not _COLOR:
        return diff
    out: list[str] = []
    for line in diff.splitlines():
        if line.startswith(("+++", "---")):
            out.append(f"{BOLD}{line}{RST}")
        elif line.startswith("@@"):
            out.append(f"{CYAN}{line}{RST}")
        elif line.startswith("+"):
            out.append(f"{GREEN}{line}{RST}")
        elif line.startswith("-"):
            out.append(f"{RED}{line}{RST}")
        else:
            out.append(line)
    return "\n".join(out)


def format_diff_box(repo_name: str, diff: str, width: int | None = None) -> str:
    """Wrap a diff in a unicode box with repo name header and line wrapping."""
    if width is None:
        width = min(term_width(), 120)
    inner = width - 4  # "│ " + content + " │"
    colored_diff = format_diff(diff)

    def box_line(content: str) -> str:
        stripped = content.rstrip()
        pad = max(0, inner - visible_len(stripped))
        return f"{DIM}│{RST} {stripped}{' ' * pad} {DIM}│{RST}"

    border = "─" * (width - 2)
    lines: list[str] = [
        f"{DIM}╭{border}╮{RST}",
        box_line(f"{BOLD}{repo_name}{RST}"),
        f"{DIM}├{border}┤{RST}",
    ]
    for dl in colored_diff.splitlines():
        lines.extend(box_line(w) for w in wrap_colored_line(dl, inner))
    lines.append(f"{DIM}╰{border}╯{RST}")
    return "\n".join(lines)


@contextmanager
def spinner(message: str) -> Generator[None]:
    """Show a spinner with a message."""
    if not _COLOR:
        print(message)
        yield
        return

    done = threading.Event()

    def spin() -> None:
        i = 0
        while not done.is_set():
            frame = _FRAMES[i % len(_FRAMES)]
            sys.stdout.write(f"\r{_EL}\033[2m{frame}\033[0m {message}")
            sys.stdout.flush()
            i += 1
            done.wait(0.08)
        sys.stdout.write(f"\r{_EL}\033[32m✓\033[0m {message} \033[2mdone\033[0m\n")
        sys.stdout.flush()

    t = threading.Thread(target=spin, daemon=True)
    t.start()
    try:
        yield
    finally:
        done.set()
        t.join()


class ProgressDisplay:
    """Thread-safe progress display with a fixed footer.

    Cursor invariant: when footer is drawn, cursor is at end of the last
    footer line (progress counter). _erase_footer leaves cursor at the
    first footer line position, ready for new content.
    """

    def __init__(
        self,
        total: int,
        workers: int,
        label: str = "",
        done: int = 0,
    ) -> None:
        self._total = total
        self._label = label
        self._lock = threading.Lock()
        self._slots: dict[str, str] = {}
        self._height = workers + 1  # always >= 2
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._frame_idx = 0
        self._counts = {"done": done, "failed": 0}
        self._initial_done = done  # pre-existing done count before this run
        self._completed: set[str] = set()  # repo names that called finish()
        self._footer_drawn = False

    def __enter__(self) -> ProgressDisplay:
        if _COLOR:
            sys.stdout.write("\033[?25l")
            sys.stdout.write("\n" * self._height)
            sys.stdout.write(f"\033[{self._height}A\r")
            sys.stdout.flush()
            self._draw_footer()
            self._thread = threading.Thread(target=self._spin, daemon=True)
            self._thread.start()
        return self

    def __exit__(self, *args: object) -> None:
        self._stop.set()
        if self._thread:
            self._thread.join()
        if _COLOR:
            # Spinner thread is stopped and joined — no concurrent writes.
            # Lock ensures any in-progress _write_above completed.
            with self._lock:
                self._erase_footer()
                sys.stdout.write("\033[?25h\n")
                sys.stdout.flush()

    def _move_to_footer_top(self) -> None:
        """Move cursor from end of progress line to first footer line."""
        sys.stdout.write(f"\r\033[{self._height - 1}A")

    def _erase_footer(self) -> None:
        """Erase footer. Cursor ends at first footer line position."""
        if not self._footer_drawn:
            return
        self._move_to_footer_top()
        sys.stdout.write(f"{_EL}\n" * self._height)
        sys.stdout.write(f"\033[{self._height}A\r")
        self._footer_drawn = False

    def _draw_footer(self) -> None:
        """Draw footer starting at current cursor position.

        Cursor ends at end of progress line.
        """
        frame = _FRAMES[self._frame_idx % len(_FRAMES)]
        slots = sorted(self._slots.items())

        buf: list[str] = []
        for i in range(self._height - 1):
            buf.append(f"\r{_EL}")
            if i < len(slots):
                short = slots[i][0].split("/", 1)[-1]
                buf.append(f"  \033[2m{frame} {short} ({slots[i][1]})\033[0m")
            buf.append("\n")

        c = self._counts
        total = c["done"] + c["failed"]
        parts = [f"\033[32m{c['done']} done\033[0m"]
        if c["failed"]:
            parts.append(f"\033[31m{c['failed']} failed\033[0m")
        status_line = f"  {', '.join(parts)} \033[2m({total}/{self._total})\033[0m"
        if self._label:
            status_line += f"  \033[33m{self._label}\033[0m"
        buf.append(f"\r{_EL}{status_line}")

        sys.stdout.write("".join(buf))
        sys.stdout.flush()
        self._footer_drawn = True

    def _spin(self) -> None:
        while not self._stop.is_set():
            with self._lock:
                self._frame_idx += 1
                if self._footer_drawn:
                    self._move_to_footer_top()
                    self._draw_footer()
            self._stop.wait(0.08)

    def _write_above(self, text: str) -> None:
        """Write text above the footer. Handles both COLOR and non-COLOR."""
        if _COLOR:
            self._erase_footer()
            sys.stdout.write(f"{text}\n")
            sys.stdout.flush()
            self._draw_footer()
        else:
            sys.stdout.write(f"{text}\n")
            sys.stdout.flush()

    def set_phase(self, repo_name: str, phase: str) -> None:
        with self._lock:
            self._slots[repo_name] = phase

    def print(self, text: str) -> None:
        """Print text above the footer."""
        with self._lock:
            self._write_above(text)

    def finish(self, repo_name: str, msg: str = "", category: str = "done") -> None:
        """Mark a repo as done and optionally print its result line."""
        with self._lock:
            self._slots.pop(repo_name, None)
            self._completed.add(repo_name)
            if category in self._counts:
                self._counts[category] += 1
            if msg:
                self._write_above(f"  {repo_name}  {msg}")

    def cancel(self, repo_name: str) -> None:
        """Remove a repo from active display without counting it as done.

        Used for interrupted repos — they shouldn't inflate the done counter.
        """
        with self._lock:
            self._slots.pop(repo_name, None)

    def is_completed(self, repo_name: str) -> bool:
        return repo_name in self._completed

    @property
    def counts(self) -> dict[str, int]:
        return dict(self._counts)

    @property
    def initial_done(self) -> int:
        return self._initial_done
