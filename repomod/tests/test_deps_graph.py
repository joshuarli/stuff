from __future__ import annotations

import argparse
import json
import socket
from collections.abc import Sequence
from pathlib import Path
from typing import cast

import pytest

from repomod.catalog import Catalog, load_catalog
from repomod.cmd import common, deps
from repomod.dependencies import graph as graph_module
from repomod.dependencies import snapshot
from repomod.dependencies.analysis import report, report_text
from repomod.dependencies.model import (
    Component,
    Ecosystem,
    Edge,
    IndexEntry,
    Instance,
    Project,
    Relation,
    RepositoryData,
    Resolution,
    Scope,
    SourceKind,
    purl,
)

DOCUMENT = "version = 1\n\n[workspace]\nroot = \"~/repos\"\npersonal_owner = \"acme\"\n\n"


def _catalog(tmp_path: Path, entries: str) -> Catalog:
    path = tmp_path / "catalog.toml"
    path.write_text(DOCUMENT + entries, encoding="utf-8")
    return load_catalog(path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")})


def _instance(
    ecosystem: Ecosystem,
    name: str,
    version: str | None,
    *,
    context: str = "",
    source_kind: SourceKind = SourceKind.REGISTRY,
    first_party: bool = False,
    vcs_url: str | None = None,
    repository_url: str | None = None,
) -> Instance:
    component = purl(ecosystem, name)
    purl_value = purl(ecosystem, name, version)
    return Instance(
        f"{purl_value}#{context}",
        purl_value,
        component,
        version,
        source_kind,
        context=context,
        vcs_url=vcs_url,
        repository_url=repository_url,
        first_party=first_party,
    )


def _project(directory: str, ecosystem: Ecosystem) -> Project:
    return Project(f"{directory}:{ecosystem.value}", ecosystem, directory)


def _components(instances: Sequence[Instance]) -> tuple[Component, ...]:
    components: dict[str, Component] = {}
    for instance in instances:
        name = instance.purl.split("/", 1)[1].split("@", 1)[0].split("?", 1)[0]
        components.setdefault(
            instance.component, Component(instance.component, _ecosystem(instance), name, name)
        )
    return tuple(components.values())


def _ecosystem(instance: Instance) -> Ecosystem:
    purl_type = instance.purl.removeprefix("pkg:").split("/", 1)[0]
    return Ecosystem(purl_type)


def _edge(
    source: str,
    target: Instance,
    *,
    relation: Relation = Relation.DIRECT,
    scope: Scope = Scope.RUNTIME,
    resolution: Resolution = Resolution.RESOLVED,
    constraint: str | None = None,
) -> Edge:
    return Edge(
        from_id=source,
        to_id=target.id,
        relation=relation,
        scope=scope,
        optional=False,
        constraint=constraint,
        resolved_version=target.version,
        condition=None,
        source_kind=target.source_kind,
        resolution=resolution,
    )


def _repository(
    name: str,
    tags: tuple[str, ...],
    projects: Sequence[Project],
    instances: Sequence[Instance],
    edges: Sequence[Edge],
) -> RepositoryData:
    return RepositoryData(
        repository=name,
        tags=tags,
        inputs=(),
        inputs_fingerprint=snapshot.fingerprint(()),
        projects=tuple(projects),
        components=_components(instances),
        instances=tuple(instances),
        edges=tuple(edges),
    )


def _write_inventory(tmp_path: Path, repositories: Sequence[RepositoryData]) -> Path:
    inventory = tmp_path / "inventory"
    inventory.mkdir()
    entries = {}
    for data in repositories:
        path = snapshot.repository_file(inventory, data.repository)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(snapshot.serialize(snapshot.repository_dict(data)), encoding="utf-8")
        entries[data.repository] = IndexEntry("a" * 40, data.inputs_fingerprint, data.state)
    (inventory / "index.json").write_text(
        snapshot.serialize(snapshot.index_dict(entries, snapshot.PARSER_VERSION)),
        encoding="utf-8",
    )
    return inventory


def _graph(
    tmp_path: Path, repositories: Sequence[RepositoryData], catalog: Catalog | None = None
) -> graph_module.Graph:
    inventory = graph_module.load_inventory(_write_inventory(tmp_path, repositories))
    return graph_module.build_graph(inventory, catalog)


def _tool_repository() -> RepositoryData:
    project = _project(".", Ecosystem.CARGO)
    serde = _instance(Ecosystem.CARGO, "serde", "1.0.0")
    tokio = _instance(Ecosystem.CARGO, "tokio", "1.40.0")
    return _repository(
        "acme/tool",
        ("tool",),
        (project,),
        (serde, tokio),
        (
            _edge(project.id, serde),
            _edge(serde.id, tokio, relation=Relation.TRANSITIVE),
        ),
    )


def test_why_finds_a_path_through_a_workspace(tmp_path: Path) -> None:
    built = _graph(tmp_path, [_tool_repository()])

    result = graph_module.why(built, graph_module.parse_query("tokio"))

    assert result.matched == ("pkg:cargo/tokio@1.40.0#",)
    assert len(result.paths) == 1
    path = result.paths[0]
    assert path.project == ".:cargo"
    assert path.repository == "acme/tool"
    assert [built.label(node) for node in path.nodes] == [
        ".:cargo",
        "serde@1.0.0",
        "tokio@1.40.0",
    ]
    assert [edge.scope for edge in path.edges] == [Scope.RUNTIME, Scope.RUNTIME]
    assert result.truncated is False


def _shared_project_ids() -> list[RepositoryData]:
    project = _project(".", Ecosystem.CARGO)
    serde = _instance(Ecosystem.CARGO, "serde", "1.0.0")
    tokio = _instance(Ecosystem.CARGO, "tokio", "1.40.0")
    log = _instance(Ecosystem.CARGO, "log", "0.4.0")
    return [
        _repository(
            "acme/tool",
            ("tool",),
            (project,),
            (serde, tokio),
            (
                _edge(project.id, serde),
                _edge(serde.id, tokio, relation=Relation.TRANSITIVE),
            ),
        ),
        _repository(
            "acme/zebra",
            ("tool",),
            (project,),
            (serde, log),
            (_edge(project.id, serde), _edge(project.id, log)),
        ),
    ]


def test_repositories_sharing_project_ids_keep_their_own_edges(tmp_path: Path) -> None:
    built = _graph(tmp_path, _shared_project_ids())

    tokio = graph_module.why(built, graph_module.parse_query("tokio"))
    assert [path.repository for path in tokio.paths] == ["acme/tool"]

    serde = graph_module.consumers(built, graph_module.parse_query("serde"))
    assert [(item.repository, item.source) for item in serde] == [
        ("acme/tool", ".:cargo"),
        ("acme/zebra", ".:cargo"),
    ]
    log = graph_module.consumers(built, graph_module.parse_query("log"))
    assert [item.repository for item in log] == ["acme/zebra"]

    concentration = cast(dict[str, object], report(built)["concentration"])
    rows = {
        str(row["name"]): row for row in cast(list[dict[str, object]], concentration["rows"])
    }
    assert cast(dict[str, int], rows["serde"]["direct_repositories"])["value"] == 2
    assert cast(dict[str, int], rows["tokio"]["all_repositories"])["value"] == 1


def test_package_query_parsing() -> None:
    assert graph_module.parse_query("serde") == graph_module.PackageQuery(
        "serde", None, "serde", None
    )
    parsed = graph_module.parse_query("cargo:serde@1.0.0")
    assert parsed.ecosystem is Ecosystem.CARGO
    assert parsed.name == "serde"
    assert parsed.version == "1.0.0"
    scoped = graph_module.parse_query("npm:@scope/pkg@1.2.3")
    assert scoped.ecosystem is Ecosystem.NPM
    assert scoped.name == "@scope/pkg"
    assert scoped.version == "1.2.3"
    scoped_without_version = graph_module.parse_query("@scope/pkg")
    assert scoped_without_version.name == "@scope/pkg"
    assert scoped_without_version.version is None


def test_reverse_consumer_query(tmp_path: Path) -> None:
    built = _graph(tmp_path, [_tool_repository()])

    tokio_consumers = graph_module.consumers(built, graph_module.parse_query("tokio"))
    assert len(tokio_consumers) == 1
    assert tokio_consumers[0].source == "pkg:cargo/serde@1.0.0#"
    assert tokio_consumers[0].direct is False

    serde_consumers = graph_module.consumers(built, graph_module.parse_query("serde"))
    assert len(serde_consumers) == 1
    assert serde_consumers[0].source == ".:cargo"
    assert serde_consumers[0].direct is True


def test_cycles_do_not_loop(tmp_path: Path) -> None:
    project = _project(".", Ecosystem.CARGO)
    a = _instance(Ecosystem.CARGO, "a", "1.0.0")
    b = _instance(Ecosystem.CARGO, "b", "1.0.0")
    target = _instance(Ecosystem.CARGO, "target", "1.0.0")
    repository = _repository(
        "acme/tool",
        ("tool",),
        (project,),
        (a, b, target),
        (
            _edge(project.id, a, resolution=Resolution.RESOLVED),
            _edge(a.id, b, relation=Relation.TRANSITIVE),
            _edge(b.id, a, relation=Relation.TRANSITIVE),
            _edge(b.id, target, relation=Relation.TRANSITIVE),
        ),
    )
    built = _graph(tmp_path, [repository])

    result = graph_module.why(built, graph_module.parse_query("target"))

    assert len(result.paths) == 1
    assert result.paths[0].nodes == (".:cargo", a.id, b.id, target.id)


def test_max_paths_truncates(tmp_path: Path) -> None:
    project = _project(".", Ecosystem.CARGO)
    left = _instance(Ecosystem.CARGO, "left", "1.0.0")
    right = _instance(Ecosystem.CARGO, "right", "1.0.0")
    target = _instance(Ecosystem.CARGO, "target", "1.0.0")
    repository = _repository(
        "acme/tool",
        ("tool",),
        (project,),
        (left, right, target),
        (
            _edge(project.id, left),
            _edge(project.id, right),
            _edge(left.id, target, relation=Relation.TRANSITIVE),
            _edge(right.id, target, relation=Relation.TRANSITIVE),
        ),
    )
    built = _graph(tmp_path, [repository])

    result = graph_module.why(built, graph_module.parse_query("target"), max_paths=1)

    assert len(result.paths) == 1
    assert result.truncated is True


def test_first_party_git_link_and_near_name(tmp_path: Path) -> None:
    catalog = _catalog(
        tmp_path,
        '[repos."acme"]\ntool = ["tool"]\nlib = ["library"]\npublished = ["library", "crates-io"]\n',
    )
    project = _project(".", Ecosystem.CARGO)
    linked = _instance(
        Ecosystem.CARGO,
        "lib",
        "1.0.0",
        source_kind=SourceKind.GIT,
        vcs_url="https://github.com/acme/lib",
    )
    near = _instance(
        Ecosystem.CARGO,
        "libx",
        "1.0.0",
        source_kind=SourceKind.GIT,
        vcs_url="https://github.com/acme/libx",
    )
    local = _instance(
        Ecosystem.CARGO,
        "local",
        "0.1.0",
        source_kind=SourceKind.PATH,
        first_party=True,
    )
    published = _instance(Ecosystem.CARGO, "published", "1.0.0")
    near_published = _instance(Ecosystem.CARGO, "published-extra", "1.0.0")
    publisher = _project(".", Ecosystem.CARGO)
    publisher_package = _instance(
        Ecosystem.CARGO,
        "published",
        "0.1.0",
        source_kind=SourceKind.WORKSPACE,
        first_party=True,
    )
    repositories = (
        _repository(
            "acme/tool",
            ("tool",),
            (project,),
            (linked, near, local, published, near_published),
            (
                _edge(project.id, linked),
                _edge(project.id, near),
                _edge(project.id, local),
                _edge(project.id, published),
                _edge(project.id, near_published),
            ),
        ),
        _repository(
            "acme/published",
            ("library", "crates-io"),
            (publisher,),
            (publisher_package,),
            (),
        ),
    )
    built = _graph(tmp_path, repositories, catalog)

    assert graph_module.is_first_party(built, linked) is True
    assert graph_module.is_first_party(built, near) is False
    assert graph_module.is_first_party(built, local) is True
    assert graph_module.is_first_party(built, published) is True
    assert graph_module.is_first_party(built, near_published) is False


def test_first_party_native_registry(tmp_path: Path) -> None:
    native = next(iter(graph_module._native_registry()), None)
    if native is None:
        pytest.skip("no native cargo registry configured")
    catalog = _catalog(tmp_path, '[repos."acme"]\ntool = ["tool"]\n')
    project = _project(".", Ecosystem.CARGO)
    native_instance = _instance(
        Ecosystem.CARGO, "laputa-thing", "1.0.0", repository_url=native
    )
    other_instance = _instance(
        Ecosystem.CARGO, "laputa-thing", "1.0.0", repository_url="https://example.com/"
    )
    built = _graph(
        tmp_path,
        [
            _repository(
                "acme/tool",
                ("tool",),
                (project,),
                (native_instance, other_instance),
                (_edge(project.id, native_instance), _edge(project.id, other_instance)),
            )
        ],
        catalog,
    )

    assert graph_module.is_first_party(built, native_instance) is True
    assert graph_module.is_first_party(built, other_instance) is False


def test_divergence_across_and_within_repositories(tmp_path: Path) -> None:
    project = _project(".", Ecosystem.CARGO)
    serde_one = _instance(Ecosystem.CARGO, "serde", "1.0.0")
    serde_two = _instance(Ecosystem.CARGO, "serde", "2.0.0")
    first = _repository(
        "acme/a",
        ("tool",),
        (project,),
        (serde_one, serde_two),
        (_edge(project.id, serde_one), _edge(project.id, serde_two)),
    )
    second_project = _project(".", Ecosystem.CARGO)
    second = _repository(
        "acme/b",
        ("tool",),
        (second_project,),
        (_instance(Ecosystem.CARGO, "serde", "2.0.0"),),
        (),
    )
    built = _graph(tmp_path, [first, second])

    value = report(built)
    divergence = _dict(value["divergence"])
    fleet = _list(divergence["packages"])
    serde = next(row for row in fleet if _dict(row)["component"] == "pkg:cargo/serde")
    assert sorted(
        str(_dict(version)["version"]) for version in _list(_dict(serde)["versions"])
    ) == [
        "1.0.0",
        "2.0.0",
    ]
    within = _list(divergence["repositories"])
    assert _dict(within[0])["repository"] == "acme/a"
    assert _dict(within[0])["component"] == "pkg:cargo/serde"


def _walk_metrics(value: object) -> None:
    if isinstance(value, dict):
        if "value" in value:
            unit = value.get("unit")
            denominator = value.get("denominator")
            assert isinstance(unit, str)
            assert unit
            assert isinstance(denominator, str)
            assert denominator
        for item in value.values():
            _walk_metrics(item)
    elif isinstance(value, list):
        for item in value:
            _walk_metrics(item)


def test_every_metric_has_a_unit_and_denominator(tmp_path: Path) -> None:
    built = _graph(tmp_path, [_tool_repository()])

    value = report(built)

    _walk_metrics(value)
    composition = _dict(value["composition"])
    assert _dict(composition["repositories"])["value"] == 1
    assert _dict(composition["projects"])["value"] == 1
    assert _dict(composition["package_names"])["value"] == 2
    assert _dict(composition["name_versions"])["value"] == 2
    assert _dict(composition["direct_edges"])["value"] == 1
    assert _dict(composition["transitive_edges"])["value"] == 1


def test_report_text_renders_sections(tmp_path: Path) -> None:
    built = _graph(tmp_path, [_tool_repository()])

    lines = report_text(report(built))

    assert lines[0] == "composition"
    assert any(line.startswith("concentration") for line in lines)
    assert any(line.startswith("divergence:") for line in lines)
    assert any(line.startswith("source risk") for line in lines)
    assert any(line.startswith("first-party links:") for line in lines)
    assert any(line.startswith("freshness:") for line in lines)


def test_graph_and_report_do_not_open_sockets(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    built = _graph(tmp_path, [_tool_repository()])

    def blocked(*arguments: object, **keywords: object) -> None:
        raise AssertionError("network access is not allowed")

    monkeypatch.setattr(socket.socket, "connect", blocked)
    monkeypatch.setattr(socket, "create_connection", blocked)

    assert graph_module.why(built, graph_module.parse_query("tokio")).paths
    assert report(built)["composition"]


def test_why_cli_exit_codes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    repositories = [_tool_repository()]
    inventory = _write_inventory(tmp_path, repositories)
    catalog = _catalog(tmp_path, '[repos."acme"]\ntool = ["tool"]\n')
    monkeypatch.setattr(deps, "_inventory", lambda: inventory)
    monkeypatch.setattr(common, "default_catalog", lambda: catalog)

    found = argparse.Namespace(
        operation="why", package="tokio", repo=[], tag=[], max_paths=20, json=False, include_private=False
    )
    assert deps._deps(found) == 0
    assert "tokio@1.40.0" in capsys.readouterr().out

    missing = argparse.Namespace(
        operation="why", package="missing", repo=[], tag=[], max_paths=20, json=False, include_private=False
    )
    assert deps._deps(missing) == 1
    assert "no matching package" in capsys.readouterr().out


def test_report_cli_json_is_deterministic(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    inventory = _write_inventory(tmp_path, [_tool_repository()])
    catalog = _catalog(tmp_path, '[repos."acme"]\ntool = ["tool"]\n')
    monkeypatch.setattr(deps, "_inventory", lambda: inventory)
    monkeypatch.setattr(common, "default_catalog", lambda: catalog)
    args = argparse.Namespace(operation="report", include_private=False, repo=[], tag=[], json=True)

    assert deps._deps(args) == 0
    first = capsys.readouterr().out
    assert deps._deps(args) == 0
    second = capsys.readouterr().out
    assert first == second
    assert json.loads(first)["composition"]["repositories"]["value"] == 1


def _dict(value: object) -> dict[str, object]:
    assert isinstance(value, dict)
    return cast(dict[str, object], value)


def _list(value: object) -> list[object]:
    assert isinstance(value, list)
    return cast(list[object], value)
