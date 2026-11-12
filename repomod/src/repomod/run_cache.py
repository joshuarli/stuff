from __future__ import annotations

from collections.abc import Callable, Generator, Hashable
from concurrent.futures import Future
from contextlib import contextmanager
from contextvars import ContextVar
from threading import Lock
from typing import cast

_CACHE: ContextVar[RunCache | None] = ContextVar("repomod_run_cache", default=None)


class RunCache:
    def __init__(self) -> None:
        self._futures: dict[Hashable, Future[object]] = {}
        self._lock = Lock()

    def get_or_compute[T](self, key: Hashable, factory: Callable[[], T]) -> T:
        with self._lock:
            future = self._futures.get(key)
            owner = future is None
            if future is None:
                future = Future()
                self._futures[key] = future
        if owner:
            try:
                result = factory()
            except BaseException as error:
                future.set_exception(error)
                raise
            future.set_result(result)
            return result
        return cast(T, future.result())


@contextmanager
def run_cache() -> Generator[None]:
    token = _CACHE.set(RunCache())
    try:
        yield
    finally:
        _CACHE.reset(token)


def cached[T](key: Hashable, factory: Callable[[], T]) -> T:
    cache = _CACHE.get()
    if cache is None:
        return factory()
    return cache.get_or_compute(key, factory)
