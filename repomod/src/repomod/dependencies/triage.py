from __future__ import annotations

from collections import defaultdict
from collections.abc import Collection, Iterable, Sequence
from dataclasses import dataclass
from enum import Enum

from repomod.dependencies import insights
from repomod.dependencies.insights import Gap
from repomod.dependencies.model import Component, Ecosystem

_ADVISORIES_SHOWN = 2
ROUTINE_COMMAND = "uv run repomod fix --rule audit-upgrades"
TOOLING_COMMAND = "uv run repomod fix --rule python-standard"


class ActionKind(Enum):
    SECURITY = "security"
    UNREADABLE = "unreadable"
    LOCKFILE = "lockfile"
    BREAKING = "breaking"
    TOOLING = "tooling"
    ROUTINE = "routine"

    @property
    def rank(self) -> int:
        return list(ActionKind).index(self)


@dataclass(frozen=True, slots=True)
class Action:
    kind: ActionKind
    subject: str
    purl: str | None
    change: str
    reason: str
    repositories: tuple[str, ...]
    step: str
    command: str | None = None
    details: tuple[str, ...] = ()
    blocked: bool = False


def actions(
    vulnerable: Iterable[insights.Vulnerable],
    outdated: Sequence[insights.Outdated],
    forks: Collection[str],
    unreadable: Sequence[str],
    missing_locks: Sequence[str],
    tools: Collection[str],
) -> tuple[Action, ...]:
    is_tool = {
        row.component.purl: row.component.ecosystem is Ecosystem.PYPI and row.component.name in tools
        for row in outdated
    }
    found = [
        *_security(vulnerable, forks),
        *_upgrades(
            [row for row in outdated if row.gap is Gap.BREAKING and not is_tool[row.component.purl]],
        ),
        *_tooling([row for row in outdated if is_tool[row.component.purl]]),
        *_routine(
            [row for row in outdated if row.gap is not Gap.BREAKING and not is_tool[row.component.purl]]
        ),
    ]
    if unreadable:
        found.append(
            Action(
                ActionKind.UNREADABLE,
                "Unreadable checkouts",
                None,
                "",
                "Dependencies in these repositories are unknown",
                tuple(unreadable),
                "Restore the checkout, then re-snapshot",
                "uv run repomod deps snapshot",
            )
        )
    if missing_locks:
        found.append(
            Action(
                ActionKind.LOCKFILE,
                "Missing lockfiles",
                None,
                "",
                "Exact versions are unknown, so vulnerabilities can be missed",
                tuple(missing_locks),
                "Generate and commit a lockfile",
            )
        )
    return tuple(
        sorted(
            found,
            key=lambda action: (
                action.kind.rank,
                action.blocked,
                -len(action.repositories),
                action.subject.casefold(),
            ),
        )
    )


def _security(
    rows: Iterable[insights.Vulnerable], forks: Collection[str]
) -> Iterable[Action]:
    grouped: dict[tuple[Ecosystem, str], list[insights.Vulnerable]] = defaultdict(list)
    for row in rows:
        if {*row.direct_in, *row.transitive_in} - set(forks):
            grouped[(row.component.ecosystem, row.component.name)].append(row)
    for versions in grouped.values():
        yield _security_action(versions, forks)


def _security_action(rows: Sequence[insights.Vulnerable], forks: Collection[str]) -> Action:
    component = rows[0].component
    versions = sorted({row.version for row in rows})
    target = _upgrade_target(component, rows)
    advisories = tuple(sorted({advisory for row in rows for advisory in row.advisories}))
    direct = {repository for row in rows for repository in row.direct_in} - set(forks)
    repositories = direct | {repository for row in rows for repository in row.transitive_in} - set(forks)
    shown = ", ".join(advisories[:_ADVISORIES_SHOWN])
    extra = len(advisories) - _ADVISORIES_SHOWN
    if target is None:
        change = f"{', '.join(versions)}, no newer release"
        step = "Replace it or accept the risk"
    else:
        change = f"{', '.join(versions)} → {target}"
        step = f"Upgrade the requirement to {target}" if direct else f"Update the lockfile to {target}"
    return Action(
        ActionKind.SECURITY,
        component.display_name,
        component.purl,
        change,
        shown + (f" +{extra} more" if extra > 0 else ""),
        tuple(sorted(repositories, key=str.casefold)),
        step,
        details=advisories,
        blocked=target is None,
    )


def _upgrade_target(component: Component, rows: Sequence[insights.Vulnerable]) -> str | None:
    newest = max(
        (
            parsed
            for parsed in (insights.release(component.ecosystem, row.version) for row in rows)
            if parsed is not None
        ),
        default=None,
    )
    latest = next((row.latest for row in rows if row.latest is not None), None)
    if latest is None or newest is None:
        return None
    parsed_latest = insights.release(component.ecosystem, latest)
    if parsed_latest is None or parsed_latest <= newest:
        return None
    return latest


def _upgrades(rows: Sequence[insights.Outdated]) -> Iterable[Action]:
    for row in rows:
        yield Action(
            ActionKind.BREAKING,
            row.component.display_name,
            row.component.purl,
            f"{_versions(row)} → {row.latest}",
            f"Breaking upgrade in {row.component.ecosystem.value}",
            _repositories([row]),
            "Edit the requirement and migrate",
        )


def _tooling(rows: Sequence[insights.Outdated]) -> Iterable[Action]:
    if rows:
        yield Action(
            ActionKind.TOOLING,
            ", ".join(sorted({row.component.name for row in rows}, key=str.casefold)),
            None,
            "",
            "Centrally pinned Python tools",
            _repositories(rows),
            "Align with the approved pins",
            TOOLING_COMMAND,
        )


def _routine(rows: Sequence[insights.Outdated]) -> Iterable[Action]:
    if rows:
        yield Action(
            ActionKind.ROUTINE,
            f"{len(rows)} minor and patch upgrades",
            None,
            "",
            "Compatible updates, applied one pull request per repository",
            _repositories(rows),
            "Run the fixer, which opens pull requests",
            ROUTINE_COMMAND,
        )


def _versions(row: insights.Outdated) -> str:
    return ", ".join(use.version for use in row.uses)


def _repositories(rows: Iterable[insights.Outdated]) -> tuple[str, ...]:
    return tuple(
        sorted(
            {repository for row in rows for use in row.uses for repository in use.repositories},
            key=str.casefold,
        )
    )
