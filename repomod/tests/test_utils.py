"""Tests for repomod.utils — retry logic."""

import subprocess

import pytest

from repomod.utils import retry


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


def test_retry_does_not_retry_non_retryable() -> None:
    @retry(tries=3, initial_delay=0.01)
    def bad_value() -> str:
        raise ValueError("not retryable")

    with pytest.raises(ValueError, match="not retryable"):
        bad_value()
