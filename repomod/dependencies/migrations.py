from __future__ import annotations

from dataclasses import dataclass
from collections.abc import Sequence

from repomod.dependencies import insights
from repomod.dependencies.model import Ecosystem

_FIX_LAPUTA_RUSTIX = "uv run repomod fix --rule laputa-rustix"


@dataclass(frozen=True, slots=True)
class Migration:
    name: str
    target: str | None
    reason: str
    command: str | None


_MIGRATIONS = {
    (Ecosystem.CARGO, "rustix"): Migration(
        "rustix",
        "laputa-rustix",
        "The fleet prefers the laputa-systems fork of rustix; migrate the dependency instead of bumping it",
        _FIX_LAPUTA_RUSTIX,
    ),
    (Ecosystem.NPM, "oxlint-tsgolint"): Migration(
        "oxlint-tsgolint",
        None,
        "Oxlint loads tsrs-lint from the tsrs package; remove oxlint-tsgolint instead of bumping it",
        None,
    ),
}


def split(
    rows: Sequence[insights.Outdated],
) -> tuple[tuple[tuple[Migration, insights.Outdated], ...], tuple[insights.Outdated, ...]]:
    migrating: list[tuple[Migration, insights.Outdated]] = []
    rest: list[insights.Outdated] = []
    for row in rows:
        migration = _MIGRATIONS.get((row.component.ecosystem, row.component.name.casefold()))
        if migration is None:
            rest.append(row)
        else:
            migrating.append((migration, row))
    return tuple(migrating), tuple(rest)
