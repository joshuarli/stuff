"""Shared utilities: retry, rate limiting, interrupt handling."""

import logging
import os
import signal
import subprocess
import threading
import time
from collections.abc import Callable
from functools import wraps
from typing import TypeVar

logger = logging.getLogger(__name__)

T = TypeVar("T")

SUBPROCESS_TIMEOUT = 120  # seconds — prevent indefinite hangs

RETRYABLE_ERRORS = (
    subprocess.CalledProcessError,
    subprocess.TimeoutExpired,
    OSError,
)


class ShutdownError(Exception):
    """Raised when an interrupt is detected during retry/rate-limit waits."""


class InterruptHandler:
    """Encapsulates ^C handling: event and signal handler.

    Single instance shared across the application. No mutable globals.
    All waits use event.wait(timeout) so they wake instantly on ^C.
    The handler only sets the event. A second SIGINT terminates the process.
    """

    def __init__(self) -> None:
        self.event = threading.Event()
        self._original_sigint = signal.getsignal(signal.SIGINT)

    @property
    def is_set(self) -> bool:
        return self.event.is_set()

    def install(self) -> None:
        """Replace SIGINT with a handler that only sets the event.

        The handler runs on the main thread. With one worker that thread may
        already hold ProgressDisplay's lock, and a non-reentrant acquire from
        the handler never returns, so the handler installed here would never
        see a second SIGINT. The second SIGINT restores the default action and
        re-raises it, which terminates the process.
        """

        def handler(signum: int, frame: object) -> None:
            if self.event.is_set():
                signal.signal(signal.SIGINT, signal.SIG_DFL)
                os.kill(os.getpid(), signal.SIGINT)
                return
            self.event.set()

        signal.signal(signal.SIGINT, handler)

    def restore(self) -> None:
        """Restore the original SIGINT handler and clear the event."""
        signal.signal(signal.SIGINT, self._original_sigint)
        self.event.clear()

    def check(self) -> None:
        """Raise ShutdownError if interrupted."""
        if self.event.is_set():
            raise ShutdownError

    def wait(self, seconds: float) -> None:
        """Sleep that returns immediately on ^C via Event.wait()."""
        if self.event.wait(timeout=seconds):
            raise ShutdownError


# Single instance — used by retry, RateLimiter, and main.py
interrupt = InterruptHandler()


def retry(
    tries: int = 3,
    backoff: float = 2.0,
    initial_delay: float = 1.0,
) -> Callable[[Callable[..., T]], Callable[..., T]]:
    """Decorator for exponential-backoff retry on transient errors.

    Bails immediately if the global interrupt flag is set.
    """

    def decorator(fn: Callable[..., T]) -> Callable[..., T]:
        @wraps(fn)
        def wrapper(*args: object, **kwargs: object) -> T:
            delay = initial_delay
            last_exc: BaseException | None = None
            fn_name = getattr(fn, "__name__", fn.__class__.__name__)
            for attempt in range(1, tries + 1):
                interrupt.check()
                try:
                    return fn(*args, **kwargs)
                except ShutdownError:
                    raise
                except RETRYABLE_ERRORS as exc:
                    last_exc = exc
                    if attempt == tries:
                        break
                    logger.debug(
                        "%s failed (attempt %d/%d), retrying in %.1fs: %s",
                        fn_name,
                        attempt,
                        tries,
                        delay,
                        exc,
                    )
                    interrupt.wait(delay)
                    delay *= backoff
            if last_exc is None:
                raise RuntimeError("retry() exhausted without capturing an exception")
            # No interrupt.wait on the final attempt, so the child error would
            # escape even though ^C is already set.
            if interrupt.is_set:
                raise ShutdownError from last_exc
            raise last_exc

        return wrapper

    return decorator


class RateLimiter:
    """Thread-safe rate limiter. Enforces minimum delay between GitHub API calls."""

    def __init__(self, min_delay: float = 0.25) -> None:
        self._min_delay = min_delay
        self._lock = threading.Lock()
        self._last_call = 0.0

    def __enter__(self) -> None:
        interrupt.check()
        self._lock.acquire()
        elapsed = time.monotonic() - self._last_call
        if elapsed < self._min_delay:
            remaining = self._min_delay - elapsed
            if interrupt.event.wait(timeout=remaining):
                self._lock.release()
                raise ShutdownError

    def __exit__(self, *args: object) -> None:
        self._last_call = time.monotonic()
        self._lock.release()


# Global rate limiter — shared across all threads.
# 0.25s minimum between GitHub API calls (4 req/s max).
github_rate_limiter = RateLimiter(min_delay=0.25)

# Creating PRs counts as content creation, which GitHub's secondary rate limits
# police far more tightly than reads (guidance: well under 80/minute, 500/hour).
# Bursts of creates can get the token throttled, so they get their own spacing.
pr_create_limiter = RateLimiter(min_delay=2.0)
