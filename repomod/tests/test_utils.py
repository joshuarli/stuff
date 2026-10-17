"""Tests for repomod.utils — retry logic."""

import os
import signal
import subprocess
from collections.abc import Callable
from types import FrameType
from typing import cast
from unittest.mock import patch

import pytest

from repomod.utils import ShutdownError, interrupt, retry


def _call_handler(handler: object) -> None:
    cast("Callable[[int, FrameType | None], object]", handler)(signal.SIGINT, None)


def test_retry_retries_on_failure() -> None:
    """retry decorator should retry on CalledProcessError."""
    calls: list[int] = [0]

    @retry(tries=3, initial_delay=0.01)
    def flaky() -> str:
        calls[0] += 1
        if calls[0] < 3:
            raise subprocess.CalledProcessError(1, "cmd")
        return "ok"

    assert flaky() == "ok"
    assert calls[0] == 3


def test_retry_gives_up_after_max_tries() -> None:
    @retry(tries=2, initial_delay=0.01)
    def always_fails() -> str:
        raise subprocess.CalledProcessError(1, "cmd")

    with pytest.raises(subprocess.CalledProcessError):
        always_fails()


def test_retry_last_attempt_raises_shutdown_when_interrupted() -> None:
    """The last attempt must not re-raise the child error once ^C is set.

    Earlier attempts already stop in interrupt.wait. The final one used to
    surface CalledProcessError, which callers record as a failed repo.
    """
    interrupt.event.clear()
    calls = [0]

    @retry(tries=3, initial_delay=0.01)
    def killed() -> None:
        calls[0] += 1
        if calls[0] == 3:
            interrupt.event.set()
        raise subprocess.CalledProcessError(-2, ["git", "push"])

    try:
        with pytest.raises(ShutdownError):
            killed()
        assert calls[0] == 3
    finally:
        interrupt.event.clear()


def test_second_sigint_restores_default_handler_and_reraises() -> None:
    """The second ^C terminates. The first only sets the event.

    os.kill is patched so the re-raise does not kill the test runner.
    """
    previous = signal.getsignal(signal.SIGINT)
    interrupt.event.clear()
    try:
        interrupt.install()
        handler = signal.getsignal(signal.SIGINT)
        assert callable(handler)
        with patch("os.kill") as kill:
            _call_handler(handler)
            assert interrupt.is_set
            assert signal.getsignal(signal.SIGINT) is not signal.SIG_DFL
            kill.assert_not_called()
            _call_handler(handler)
            assert signal.getsignal(signal.SIGINT) is signal.SIG_DFL
            kill.assert_called_once_with(os.getpid(), signal.SIGINT)
    finally:
        signal.signal(signal.SIGINT, previous)
        interrupt.event.clear()


def test_retry_does_not_retry_non_retryable() -> None:
    @retry(tries=3, initial_delay=0.01)
    def bad_value() -> str:
        raise ValueError("not retryable")

    with pytest.raises(ValueError, match="not retryable"):
        bad_value()
