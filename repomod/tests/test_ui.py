"""Tests for repomod.ui — text formatting and ProgressDisplay logic."""

from unittest.mock import patch

from repomod.ui import (
    ProgressDisplay,
    format_diff,
    format_diff_box,
    strip_ansi,
    visible_len,
    wrap_colored_line,
)


def test_strip_ansi() -> None:
    assert strip_ansi("\033[32mgreen\033[0m") == "green"
    assert strip_ansi("plain") == "plain"
    assert strip_ansi("\033[1m\033[36mbold cyan\033[0m") == "bold cyan"
    assert strip_ansi("") == ""


def test_visible_len() -> None:
    assert visible_len("hello") == 5
    assert visible_len("\033[31mhello\033[0m") == 5
    assert visible_len("") == 0


def test_wrap_short_line() -> None:
    assert wrap_colored_line("short", 80) == ["short"]


def test_wrap_exact_width() -> None:
    assert wrap_colored_line("a" * 10, 10) == ["a" * 10]


def test_wrap_long_line() -> None:
    result = wrap_colored_line("a" * 25, 10)
    assert len(result) == 3
    assert result[0] == "a" * 10
    assert result[1] == "a" * 10
    assert result[2] == "a" * 5


def test_wrap_preserves_ansi() -> None:
    line = "\033[32m" + "x" * 20 + "\033[0m"
    result = wrap_colored_line(line, 10)
    assert len(result) == 2
    joined = "".join(result)
    assert strip_ansi(joined) == "x" * 20


def test_format_diff_no_color() -> None:
    with patch("repomod.ui._COLOR", False):
        diff = "+added\n-removed\n context"
        assert format_diff(diff) == diff


def test_format_diff_passthrough_no_color() -> None:
    """Without color, format_diff returns the diff unchanged."""
    diff = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n ctx"
    result = format_diff(diff)
    assert result == diff


def test_format_diff_box_structure() -> None:
    diff = "+added\n-removed"
    box = format_diff_box("acme/backend", diff, width=60)
    raw = strip_ansi(box)
    lines = raw.splitlines()

    assert lines[0].startswith("╭")
    assert lines[0].endswith("╮")
    assert "acme/backend" in lines[1]
    assert lines[1].startswith("│")
    assert lines[1].rstrip().endswith("│")
    assert lines[2].startswith("├")
    assert "added" in strip_ansi(lines[3])
    assert "removed" in strip_ansi(lines[4])
    assert lines[-1].startswith("╰")
    assert lines[-1].endswith("╯")


def test_format_diff_box_alignment() -> None:
    """All lines in the box should have the same visible width."""
    diff = "+short\n-a much longer line here"
    box = format_diff_box("acme/sdk-python", diff, width=50)
    raw_lines = strip_ansi(box).splitlines()
    widths = {len(line) for line in raw_lines}
    assert len(widths) == 1, f"inconsistent widths: {widths}"


def test_format_diff_box_wraps_long_lines() -> None:
    diff = "+" + "x" * 200
    box = format_diff_box("acme/platform", diff, width=60)
    raw_lines = strip_ansi(box).splitlines()
    content_lines = [
        ln for ln in raw_lines if ln.startswith("│") and "platform" not in ln
    ]
    assert len(content_lines) > 1


def test_format_diff_box_strips_trailing_whitespace() -> None:
    """Trailing whitespace in diff content shouldn't break the box."""
    diff = "+line with trailing spaces   \n-another   "
    box = format_diff_box("acme/backend", diff, width=60)
    raw_lines = strip_ansi(box).splitlines()
    for line in raw_lines:
        if line.startswith("│"):
            assert line.rstrip().endswith("│")


def test_progress_display_no_color() -> None:
    """Non-TTY: just prints lines, no ANSI, no crashes."""
    with patch("repomod.ui._COLOR", False):
        pd = ProgressDisplay(total=3, workers=2)
        with pd:
            pd.finish("acme/backend", "done")
            pd.print("some output")
            pd.finish("acme/frontend", "failed", "failed")


def test_progress_display_counts() -> None:
    """Verify internal counters track correctly."""
    with patch("repomod.ui._COLOR", False):
        pd = ProgressDisplay(total=5, workers=1)
        with pd:
            pd.finish("acme/backend", "ok", "done")
            pd.finish("acme/frontend", "ok", "done")
            pd.finish("acme/platform")  # no msg, silent done
            pd.finish("acme/worker", "err", "failed")

        assert pd.counts["done"] == 3
        assert pd.counts["failed"] == 1


def test_progress_display_set_phase() -> None:
    with patch("repomod.ui._COLOR", False):
        pd = ProgressDisplay(total=2, workers=2)
        with pd:
            pd.set_phase("acme/backend", "cloning")
            assert pd._slots["acme/backend"] == "cloning"
            pd.set_phase("acme/backend", "fixing")
            assert pd._slots["acme/backend"] == "fixing"
            pd.finish("acme/backend", "done")
            assert "acme/backend" not in pd._slots


def test_progress_display_finish_removes_slot() -> None:
    with patch("repomod.ui._COLOR", False):
        pd = ProgressDisplay(total=2, workers=2)
        with pd:
            pd.set_phase("acme/backend", "cloning")
            pd.set_phase("acme/frontend", "fixing")
            pd.finish("acme/backend", "done")
            assert "acme/backend" not in pd._slots
            assert "acme/frontend" in pd._slots


def test_progress_display_completed_tracking() -> None:
    """finish() adds to _completed regardless of msg or category."""
    with patch("repomod.ui._COLOR", False):
        pd = ProgressDisplay(total=4, workers=1)
        with pd:
            pd.finish("acme/backend", "ok", "done")
            pd.finish("acme/frontend")  # silent, no msg
            pd.finish("acme/worker", "err", "failed")

        assert pd.is_completed("acme/backend")
        assert pd.is_completed("acme/frontend")
        assert pd.is_completed("acme/worker")


def test_progress_display_initial_done() -> None:
    """Pre-existing done count shows in counters from the start."""
    with patch("repomod.ui._COLOR", False):
        pd = ProgressDisplay(total=10, workers=1, done=5)
        with pd:
            pd.finish("acme/backend", "ok", "done")

        assert pd.counts["done"] == 6
        assert pd.initial_done == 5


def test_progress_display_slot_removal_without_finish() -> None:
    """Directly removing a slot (for interrupted repos) doesn't affect counts."""
    with patch("repomod.ui._COLOR", False):
        pd = ProgressDisplay(total=3, workers=2)
        with pd:
            pd.set_phase("acme/backend", "cloning")
            pd.set_phase("acme/frontend", "cloning")
            pd.finish("acme/backend", "ok", "done")

            # Simulate what process_repo does on ShutdownError
            pd.cancel("acme/frontend")

        assert pd.counts["done"] == 1
        assert pd.is_completed("acme/backend")
        assert not pd.is_completed("acme/frontend")
