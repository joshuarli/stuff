from __future__ import annotations

from collections.abc import Iterable

VENDORED_COMPONENTS = frozenset({"vendor", "third_party", "third-party"})
VENDORED_MANIFESTS = frozenset({"Cargo.toml", "package.json", "go.mod"})


def vendored_roots(paths: Iterable[str]) -> tuple[str, ...]:
    roots: set[str] = set()
    for path in paths:
        parts = path.split("/")[:-1]
        for index, part in enumerate(parts):
            if part in VENDORED_COMPONENTS:
                roots.add("/".join(parts[: index + 1]))
    return tuple(sorted(roots))


def is_vendored(path: str, roots: Iterable[str]) -> bool:
    return any(path.startswith(root + "/") for root in roots)
