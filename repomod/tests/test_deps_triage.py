from __future__ import annotations

from repomod.dependencies import insights, triage
from repomod.dependencies.insights import Gap
from repomod.dependencies.model import Component, Ecosystem
from repomod.dependencies.triage import ActionKind

TOOLS = frozenset({"ruff", "ty", "uv"})


def _component(name: str, ecosystem: Ecosystem = Ecosystem.CARGO) -> Component:
    return Component(f"pkg:{ecosystem.value}/{name}", ecosystem, name, name)


def _vulnerable(
    name: str,
    version: str,
    latest: str | None,
    *,
    direct: tuple[str, ...] = (),
    locked: tuple[str, ...] = (),
    advisories: tuple[str, ...] = ("RUSTSEC-1",),
) -> insights.Vulnerable:
    return insights.Vulnerable(_component(name), version, advisories, latest, direct, locked)


def _outdated(
    name: str,
    gap: Gap,
    repositories: tuple[str, ...],
    *,
    ecosystem: Ecosystem = Ecosystem.CARGO,
) -> insights.Outdated:
    return insights.Outdated(
        _component(name, ecosystem),
        "9.0.0",
        gap,
        (insights.VersionUse("1.0.0", repositories),),
    )


def _actions(
    *,
    vulnerable: tuple[insights.Vulnerable, ...] = (),
    outdated: tuple[insights.Outdated, ...] = (),
    forks: tuple[str, ...] = (),
    unreadable: tuple[str, ...] = (),
    missing_locks: tuple[str, ...] = (),
) -> tuple[triage.Action, ...]:
    return triage.actions(vulnerable, outdated, forks, unreadable, missing_locks, TOOLS)


def test_one_package_at_two_versions_is_one_security_action() -> None:
    found = _actions(
        vulnerable=(
            _vulnerable("crypto", "0.57.0", "0.60.0", direct=("acme/one",), advisories=("GO-1",)),
            _vulnerable("crypto", "0.58.0", "0.60.0", direct=("acme/two",), advisories=("GO-1", "GO-2")),
        )
    )
    assert len(found) == 1
    assert found[0].repositories == ("acme/one", "acme/two")
    assert found[0].details == ("GO-1", "GO-2")
    assert found[0].change == "0.57.0, 0.58.0 → 0.60.0"


def test_one_package_from_two_registries_is_one_security_action() -> None:
    mirrored = Component("pkg:cargo/rustls?repository_url=mirror", Ecosystem.CARGO, "rustls", "rustls")
    found = _actions(
        vulnerable=(
            _vulnerable("rustls", "0.23.45", "0.23.46", direct=("acme/one",)),
            insights.Vulnerable(mirrored, "0.23.45", ("RUSTSEC-1",), "0.23.46", ("acme/two",), ()),
        )
    )
    assert len(found) == 1
    assert found[0].repositories == ("acme/one", "acme/two")


def test_security_actions_with_a_newer_release_rank_before_blocked_ones() -> None:
    found = _actions(
        vulnerable=(
            _vulnerable("blocked", "1.0.0", None, direct=("acme/one", "acme/two")),
            _vulnerable("fixable", "1.0.0", "2.0.0", direct=("acme/one",)),
        )
    )
    assert [action.subject for action in found] == ["fixable", "blocked"]


def test_security_actions_leave_out_forks() -> None:
    found = _actions(
        vulnerable=(
            _vulnerable("only-fork", "1.0.0", "2.0.0", direct=("acme/fork",)),
            _vulnerable("both", "1.0.0", "2.0.0", direct=("acme/fork", "acme/mine")),
        ),
        forks=("acme/fork",),
    )
    assert [action.subject for action in found] == ["both"]
    assert found[0].repositories == ("acme/mine",)


def test_latest_older_than_the_vulnerable_version_is_not_offered_as_a_fix() -> None:
    found = _actions(vulnerable=(_vulnerable("rsa", "0.10.0-rc.18", "0.9.10", direct=("acme/one",)),))
    assert found[0].change == "0.10.0-rc.18, no newer release"
    assert found[0].step == "Replace it or accept the risk"


def test_vulnerable_only_through_the_lockfile_points_at_the_lockfile() -> None:
    found = _actions(vulnerable=(_vulnerable("zip", "1.0.0", "2.0.0", locked=("acme/one",)),))
    assert found[0].step == "Update the lockfile to 2.0.0"


def test_minor_and_patch_upgrades_collapse_into_one_command() -> None:
    found = _actions(
        outdated=(
            _outdated("a", Gap.MINOR, ("acme/one",)),
            _outdated("b", Gap.PATCH, ("acme/one", "acme/two")),
        )
    )
    assert len(found) == 1
    assert found[0].kind is ActionKind.ROUTINE
    assert found[0].repositories == ("acme/one", "acme/two")
    assert found[0].command == "uv run repomod fix --rule audit-upgrades"


def test_pinned_python_tools_are_one_action_not_breaking_upgrades() -> None:
    found = _actions(
        outdated=(
            _outdated("ruff", Gap.BREAKING, ("acme/one",), ecosystem=Ecosystem.PYPI),
            _outdated("ty", Gap.BREAKING, ("acme/two",), ecosystem=Ecosystem.PYPI),
            _outdated("ruff", Gap.BREAKING, ("acme/crate",)),
        )
    )
    assert [(action.kind, action.subject) for action in found] == [
        (ActionKind.BREAKING, "ruff"),
        (ActionKind.TOOLING, "ruff, ty"),
    ]
    assert found[1].command == "uv run repomod fix --rule python-standard"


def test_actions_rank_by_urgency_then_repositories_fixed() -> None:
    found = _actions(
        vulnerable=(_vulnerable("vuln", "1.0.0", "2.0.0", direct=("acme/one",)),),
        outdated=(
            _outdated("narrow", Gap.BREAKING, ("acme/one",)),
            _outdated("wide", Gap.BREAKING, ("acme/one", "acme/two")),
        ),
        unreadable=("acme/gone",),
        missing_locks=("acme/one",),
    )
    assert [(action.kind, action.subject) for action in found] == [
        (ActionKind.SECURITY, "vuln"),
        (ActionKind.UNREADABLE, "Unreadable checkouts"),
        (ActionKind.LOCKFILE, "Missing lockfiles"),
        (ActionKind.BREAKING, "wide"),
        (ActionKind.BREAKING, "narrow"),
    ]
