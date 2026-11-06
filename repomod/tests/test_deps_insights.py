from __future__ import annotations

from pathlib import Path

import pytest
from deps_fixtures import build_graph, instance, project, repository

from repomod.dependencies import insights
from repomod.dependencies.insights import Gap
from repomod.dependencies.model import (
    Ecosystem,
    Edge,
    Instance,
    Relation,
    Resolution,
    Scope,
    SourceKind,
)


def _edge(source: str, target: Instance, relation: Relation = Relation.DIRECT) -> Edge:
    return Edge(
        source,
        target.id,
        relation,
        Scope.RUNTIME,
        False,
        None,
        target.version,
        None,
        target.source_kind,
        Resolution.RESOLVED,
    )


def _app(
    name: str,
    direct: list[Instance],
    transitive: tuple[tuple[Instance, Instance], ...] = (),
):
    nodes = {item.id: item for item in direct}
    for parent, child in transitive:
        nodes[parent.id] = parent
        nodes[child.id] = child
    return repository(
        name,
        tuple(nodes.values()),
        projects=(project(".", Ecosystem.CARGO),),
        edges=(
            *(_edge(".:cargo", item) for item in direct),
            *(_edge(parent.id, child, Relation.TRANSITIVE) for parent, child in transitive),
        ),
    )


def _cargo(name: str, version: str, source_kind: SourceKind = SourceKind.REGISTRY) -> Instance:
    return instance(Ecosystem.CARGO, name, version, source_kind=source_kind)


@pytest.mark.parametrize(
    ("current", "latest", "expected"),
    [
        ("0.22.1", "0.23.1", Gap.BREAKING),
        ("1.4.0", "2.0.0", Gap.BREAKING),
        ("0.0.91", "0.0.95", Gap.BREAKING),
        ("1.4.0", "1.5.0", Gap.MINOR),
        ("0.2.2", "0.2.3", Gap.MINOR),
        ("1.4.0", "1.4.2", Gap.PATCH),
        ("1.4.2", "1.4.2", None),
        ("1.5.0", "1.4.2", None),
        ("git-main", "1.0.0", None),
    ],
)
def test_upgrade_gap_follows_the_leftmost_nonzero_component(
    current: str, latest: str, expected: Gap | None
) -> None:
    assert insights.gap(Ecosystem.CARGO, current, latest) is expected


def test_outdated_ranks_breaking_upgrades_first_and_skips_current(tmp_path: Path) -> None:
    graph = build_graph(
        tmp_path,
        [
            _app("acme/one", [_cargo("base64", "0.22.1"), _cargo("log", "0.4.30")]),
            _app("acme/two", [_cargo("log", "0.4.30"), _cargo("serde", "1.0.200")]),
        ],
    )
    enrichment = {
        "pkg:cargo/base64@0.22.1": {"latest": "0.23.0"},
        "pkg:cargo/log@0.4.30": {"latest": "0.4.35"},
        "pkg:cargo/serde@1.0.200": {"latest": "1.0.200"},
    }
    rows = insights.outdated(insights.declarations(graph), enrichment)
    assert [(row.component.name, row.gap, row.repositories) for row in rows] == [
        ("base64", Gap.BREAKING, 1),
        ("log", Gap.MINOR, 2),
    ]


def test_vulnerable_separates_declared_from_locked_repositories(tmp_path: Path) -> None:
    tokio = _cargo("tokio", "1.0.0")
    graph = build_graph(
        tmp_path,
        [
            _app("acme/direct", [tokio]),
            _app("acme/locked", [_cargo("hyper", "1.0.0")], ((_cargo("hyper", "1.0.0"), tokio),)),
        ],
    )
    enrichment = {"pkg:cargo/tokio@1.0.0": {"latest": "1.2.0", "vulns": [{"id": "RUSTSEC-1"}]}}
    [row] = insights.vulnerable(graph, insights.declarations(graph), enrichment)
    assert row.advisories == ("RUSTSEC-1",)
    assert row.direct_in == ("acme/direct",)
    assert row.transitive_in == ("acme/locked",)
    assert row.latest == "1.2.0"


def test_weight_counts_packages_only_one_direct_dependency_brings_in(tmp_path: Path) -> None:
    heavy = _cargo("heavy", "1.0.0")
    light = _cargo("light", "1.0.0")
    shared = _cargo("shared", "1.0.0")
    own_a = _cargo("own-a", "1.0.0")
    own_b = _cargo("own-b", "1.0.0")
    deep = _cargo("deep", "1.0.0")
    graph = build_graph(
        tmp_path,
        [
            _app(
                "acme/app",
                [heavy, light],
                (
                    (heavy, shared),
                    (heavy, own_a),
                    (own_a, own_b),
                    (own_b, deep),
                    (light, shared),
                ),
            )
        ],
    )
    rows = {row.component.name: row for row in insights.weights(graph, insights.declarations(graph))}
    assert (rows["heavy"].pulls, rows["heavy"].sole) == (4, 3)
    assert (rows["light"].pulls, rows["light"].sole) == (1, 0)


def test_weight_does_not_count_other_direct_dependencies(tmp_path: Path) -> None:
    outer = _cargo("outer", "1.0.0")
    inner = _cargo("inner", "1.0.0")
    graph = build_graph(tmp_path, [_app("acme/app", [outer, inner], ((outer, inner),))])
    assert insights.weights(graph, insights.declarations(graph)) == ()


def test_splits_need_two_repositories_and_skip_local_and_alpine(tmp_path: Path) -> None:
    graph = build_graph(
        tmp_path,
        [
            _app(
                "acme/one",
                [
                    _cargo("libc", "0.2.180"),
                    _cargo("solo", "1.0.0"),
                    _cargo("member", "0.1.0", source_kind=SourceKind.PATH),
                    instance(Ecosystem.APK, "git", "2.54.0-r0"),
                ],
            ),
            _app(
                "acme/two",
                [
                    _cargo("libc", "0.2.191"),
                    _cargo("solo", "1.0.0"),
                    _cargo("member", "0.2.0", source_kind=SourceKind.PATH),
                    instance(Ecosystem.APK, "git", "2.56.0-r0"),
                ],
            ),
            _app("acme/three", [_cargo("solo", "2.0.0")]),
        ],
    )
    rows = insights.splits(insights.declarations(graph), {})
    assert [(row.component.name, row.target, row.basis) for row in rows] == [
        ("solo", "2.0.0", insights.TargetBasis.NEWEST_IN_USE),
        ("libc", "0.2.191", insights.TargetBasis.NEWEST_IN_USE),
    ]
    assert rows[0].off_target == ("acme/one", "acme/two")


def test_role_use_reports_mixed_and_off_preferred_repositories(tmp_path: Path) -> None:
    graph = build_graph(
        tmp_path,
        [
            _app("acme/mixed", [_cargo("lexopt", "0.3.0"), _cargo("clap_complete", "4.0.0")]),
            _app("acme/clean", [_cargo("lexopt", "0.3.0")]),
            _app("acme/legacy", [_cargo("clap", "4.0.0")]),
        ],
    )
    roles = insights.parse_roles(
        {
            "cli-arguments": {
                "ecosystem": "cargo",
                "preferred": "lexopt",
                "options": {"lexopt": ["lexopt"], "clap": ["clap", "clap_complete"]},
            }
        },
        "roles.toml",
    )
    [use] = insights.role_uses(roles, insights.declarations(graph))
    assert use.options == {
        "lexopt": ("acme/clean", "acme/mixed"),
        "clap": ("acme/legacy", "acme/mixed"),
    }
    assert use.overlapping == ("acme/mixed",)
    assert use.off_preferred == ("acme/legacy", "acme/mixed")
    assert use.packages[("acme/mixed", "clap")] == ("clap_complete",)


def test_complements_pass_only_alongside_the_preferred_option(tmp_path: Path) -> None:
    graph = build_graph(
        tmp_path,
        [
            _app("acme/both", [_cargo("rustix", "1.0.0"), _cargo("libc", "0.2.0")]),
            _app("acme/bare", [_cargo("libc", "0.2.0")]),
            _app("acme/clean", [_cargo("rustix", "1.0.0")]),
            _app("acme/nix", [_cargo("rustix", "1.0.0"), _cargo("nix", "0.30.0")]),
        ],
    )
    roles = insights.parse_roles(
        {
            "system-calls": {
                "ecosystem": "cargo",
                "preferred": "rustix",
                "complements": ["libc"],
                "options": {"rustix": ["rustix"], "libc": ["libc"], "nix": ["nix"]},
            }
        },
        "roles.toml",
    )
    [use] = insights.role_uses(roles, insights.declarations(graph))
    assert use.overlapping == ("acme/nix",)
    assert use.off_preferred == ("acme/bare", "acme/nix")


@pytest.mark.parametrize(
    ("role", "message"),
    [
        ({"ecosystem": "cargo", "options": {"a": ["x"]}}, "at least two options"),
        ({"ecosystem": "cargo", "options": {"a": ["x"], "b": ["x"]}}, "under both a and b"),
        (
            {"ecosystem": "cargo", "preferred": "c", "options": {"a": ["x"], "b": ["y"]}},
            "not an option",
        ),
        ({"ecosystem": "cobol", "options": {"a": ["x"], "b": ["y"]}}, "unknown ecosystem"),
        ({"ecosystem": "cargo", "prefered": "a", "options": {"a": ["x"], "b": ["y"]}}, "unknown keys"),
        (
            {"ecosystem": "cargo", "complements": ["b"], "options": {"a": ["x"], "b": ["y"]}},
            "complements need a preferred option",
        ),
        (
            {
                "ecosystem": "cargo",
                "preferred": "a",
                "complements": ["a"],
                "options": {"a": ["x"], "b": ["y"]},
            },
            "complements must be other options",
        ),
        (
            {
                "ecosystem": "cargo",
                "preferred": "a",
                "complements": ["c"],
                "options": {"a": ["x"], "b": ["y"]},
            },
            "complements must be other options",
        ),
    ],
)
def test_roles_file_rejects_invalid_roles(role: dict[str, object], message: str) -> None:
    with pytest.raises(ValueError, match=message):
        insights.parse_roles({"cli": role}, "roles.toml")


def test_scorecards_rank_repositories_by_what_needs_attention(tmp_path: Path) -> None:
    tokio = _cargo("tokio", "1.0.0")
    graph = build_graph(
        tmp_path,
        [
            _app("acme/calm", [_cargo("log", "0.4.35")]),
            _app("acme/stale", [_cargo("log", "0.3.0"), _cargo("clap", "4.0.0")]),
            _app(
                "acme/exposed",
                [_cargo("log", "0.4.35"), _cargo("hyper", "1.0.0")],
                ((_cargo("hyper", "1.0.0"), tokio),),
            ),
        ],
    )
    enrichment = {
        "pkg:cargo/log@0.3.0": {"latest": "0.4.35"},
        "pkg:cargo/log@0.4.35": {"latest": "0.4.35"},
        "pkg:cargo/tokio@1.0.0": {"vulns": [{"id": "RUSTSEC-1"}]},
    }
    roles = insights.parse_roles(
        {
            "cli": {
                "ecosystem": "cargo",
                "preferred": "lexopt",
                "options": {"lexopt": ["lexopt"], "clap": ["clap"]},
            }
        },
        "roles.toml",
    )
    found = insights.declarations(graph)
    cards = insights.scorecards(
        graph.repositories,
        insights.outdated(found, enrichment),
        insights.vulnerable(graph, found, enrichment),
        insights.splits(found, enrichment),
        insights.role_uses(roles, found),
        insights.weights(graph, found),
    )
    assert [card.repository for card in cards] == ["acme/exposed", "acme/stale", "acme/calm"]
    exposed, stale, calm = cards
    assert exposed.vulnerable == 1
    assert exposed.heaviest is not None
    assert exposed.heaviest.component.name == "hyper"
    assert (stale.behind, stale.breaking, stale.off_target) == (1, 1, 1)
    assert stale.off_preferred == ("cli",)
    assert (calm.vulnerable, calm.behind, calm.off_target, calm.heaviest) == (0, 0, 0, None)


def test_split_target_prefers_comparable_versions_then_the_most_common_ref(tmp_path: Path) -> None:
    def action(ref: str) -> Instance:
        return instance(Ecosystem.GITHUB, "actions/checkout", ref)

    def toolchain(ref: str) -> Instance:
        return instance(Ecosystem.GITHUB, "dtolnay/rust-toolchain", ref)

    graph = build_graph(
        tmp_path,
        [
            _app("acme/one", [action("v4"), toolchain("master")]),
            _app("acme/two", [action("0123abcd"), toolchain("master")]),
            _app("acme/three", [action("v5"), toolchain("ffff0000")]),
        ],
    )
    targets = {
        row.component.name: (row.target, row.basis)
        for row in insights.splits(insights.declarations(graph), {})
    }
    assert targets == {
        "actions/checkout": ("v5", insights.TargetBasis.NEWEST_IN_USE),
        "dtolnay/rust-toolchain": ("master", insights.TargetBasis.MOST_COMMON),
    }
