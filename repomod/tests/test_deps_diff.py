from __future__ import annotations

import json
import socket
from pathlib import Path

import pytest
from deps_fixtures import instance, issue, repository, write_inventory
from git_helpers import git

from repomod.dependencies.diff import diff_dict, diff_inventory, diff_text
from repomod.dependencies.model import CoverageKind, Ecosystem, SourceKind


def _init(root: Path) -> None:
    root.mkdir(parents=True)
    git("init", "--initial-branch=main", str(root))


def _commit(root: Path, message: str) -> str:
    git("add", "-A", cwd=root)
    git("commit", "-m", message, cwd=root)
    return git("rev-parse", "HEAD", cwd=root)


def _inventory(root: Path, repositories) -> Path:
    return write_inventory(root, repositories)


def test_added_and_removed_instances(tmp_path: Path, git_environment: None) -> None:
    root = tmp_path / "repo"
    _init(root)
    foo = instance(Ecosystem.CARGO, "foo", "1.0.0")
    bar = instance(Ecosystem.CARGO, "bar", "2.0.0")
    _inventory(root, [repository("acme/tool", (foo,))])
    first = _commit(root, "first")
    _inventory(root, [repository("acme/tool", (foo, bar))])
    second = _commit(root, "second")
    _inventory(root, [repository("acme/tool", (bar,))])
    third = _commit(root, "third")

    added = diff_inventory(
        root, root / "data/dependencies/v1", from_rev=first, to_rev=second
    )
    assert len(added.repositories) == 1
    assert [item.purl for item in added.repositories[0].added] == [bar.purl]
    assert added.repositories[0].removed == ()

    removed = diff_inventory(
        root, root / "data/dependencies/v1", from_rev=second, to_rev=third
    )
    assert [item.purl for item in removed.repositories[0].removed] == [foo.purl]


def test_upgrade_downgrade_pep440_and_changed(
    tmp_path: Path, git_environment: None
) -> None:
    root = tmp_path / "repo"
    _init(root)
    _inventory(
        root,
        [
            repository(
                "acme/tool",
                (
                    instance(Ecosystem.CARGO, "cargo-up", "1.0.0"),
                    instance(Ecosystem.CARGO, "cargo-down", "2.0.0"),
                    instance(Ecosystem.PYPI, "python-up", "1.0.0a1"),
                    instance(Ecosystem.GENERIC, "floating", "nightly-2026-09"),
                ),
            )
        ],
    )
    first = _commit(root, "first")
    _inventory(
        root,
        [
            repository(
                "acme/tool",
                (
                    instance(Ecosystem.CARGO, "cargo-up", "1.1.0"),
                    instance(Ecosystem.CARGO, "cargo-down", "1.5"),
                    instance(Ecosystem.PYPI, "python-up", "1.0.0"),
                    instance(Ecosystem.GENERIC, "floating", "stable"),
                ),
            )
        ],
    )
    second = _commit(root, "second")

    value = diff_inventory(
        root, root / "data/dependencies/v1", from_rev=first, to_rev=second
    )

    changes = {item.name: item for item in value.repositories[0].versions}
    assert changes["cargo-up"].direction == "upgrade"
    assert changes["cargo-down"].direction == "downgrade"
    assert changes["python-up"].direction == "upgrade"
    assert changes["floating"].direction == "changed"


def test_source_change(tmp_path: Path, git_environment: None) -> None:
    root = tmp_path / "repo"
    _init(root)
    _inventory(
        root,
        [repository("acme/tool", (instance(Ecosystem.CARGO, "foo", "1.0.0"),))],
    )
    first = _commit(root, "first")
    _inventory(
        root,
        [
            repository(
                "acme/tool",
                (
                    instance(
                        Ecosystem.CARGO,
                        "foo",
                        "1.0.0",
                        source_kind=SourceKind.GIT,
                        vcs_url="https://github.com/acme/foo",
                    ),
                ),
            )
        ],
    )
    second = _commit(root, "second")

    value = diff_inventory(
        root, root / "data/dependencies/v1", from_rev=first, to_rev=second
    )

    assert len(value.repositories[0].sources) == 1
    change = value.repositories[0].sources[0]
    assert change.old is SourceKind.REGISTRY
    assert change.new is SourceKind.GIT
    assert change.new_url == "https://github.com/acme/foo"


def test_new_and_resolved_issues(tmp_path: Path, git_environment: None) -> None:
    root = tmp_path / "repo"
    _init(root)
    _inventory(
        root,
        [
            repository(
                "acme/tool",
                (instance(Ecosystem.CARGO, "foo", "1.0.0"),),
                issues=(issue(CoverageKind.MISSING_LOCK, "Cargo.lock"),),
            )
        ],
    )
    first = _commit(root, "first")
    _inventory(
        root,
        [
            repository(
                "acme/tool",
                (instance(Ecosystem.CARGO, "foo", "1.0.0"),),
                issues=(issue(CoverageKind.DYNAMIC_VALUE, "Dockerfile"),),
            )
        ],
    )
    second = _commit(root, "second")

    value = diff_inventory(
        root, root / "data/dependencies/v1", from_rev=first, to_rev=second
    )

    change = value.repositories[0]
    assert [item.kind for item in change.new_issues] == [CoverageKind.DYNAMIC_VALUE]
    assert [item.kind for item in change.resolved_issues] == [CoverageKind.MISSING_LOCK]


def test_working_tree_is_the_default_target(
    tmp_path: Path, git_environment: None
) -> None:
    root = tmp_path / "repo"
    _init(root)
    _inventory(
        root, [repository("acme/tool", (instance(Ecosystem.CARGO, "foo", "1.0.0"),))]
    )
    _commit(root, "first")
    _inventory(
        root, [repository("acme/tool", (instance(Ecosystem.CARGO, "foo", "1.1.0"),))]
    )

    value = diff_inventory(root, root / "data/dependencies/v1")

    assert value.baseline is True
    assert value.repositories[0].versions[0].direction == "upgrade"
    assert diff_text(value)[0].startswith("acme/tool")


def test_no_baseline(tmp_path: Path, git_environment: None) -> None:
    root = tmp_path / "repo"
    _init(root)
    (root / "README.md").write_text("first\n", encoding="utf-8")
    first = _commit(root, "first")
    _inventory(
        root, [repository("acme/tool", (instance(Ecosystem.CARGO, "foo", "1.0.0"),))]
    )
    second = _commit(root, "inventory")

    value = diff_inventory(
        root, root / "data/dependencies/v1", from_rev=first, to_rev=second
    )

    assert value.baseline is False
    assert value.repositories == ()
    assert diff_text(value) == ["no baseline"]
    assert diff_dict(value)["baseline"] is False


def test_diff_json_is_deterministic(tmp_path: Path, git_environment: None) -> None:
    root = tmp_path / "repo"
    _init(root)
    _inventory(
        root, [repository("acme/tool", (instance(Ecosystem.CARGO, "foo", "1.0.0"),))]
    )
    first = _commit(root, "first")
    _inventory(
        root,
        [
            repository(
                "acme/tool",
                (
                    instance(Ecosystem.CARGO, "foo", "1.1.0"),
                    instance(Ecosystem.CARGO, "bar", "2.0.0"),
                ),
            )
        ],
    )
    second = _commit(root, "second")

    value = diff_inventory(
        root, root / "data/dependencies/v1", from_rev=first, to_rev=second
    )

    assert json.dumps(diff_dict(value), sort_keys=True) == json.dumps(
        diff_dict(value), sort_keys=True
    )


def test_unknown_revision_is_rejected(tmp_path: Path, git_environment: None) -> None:
    root = tmp_path / "repo"
    _init(root)
    _inventory(root, [repository("acme/tool", ())])
    _commit(root, "first")

    with pytest.raises(ValueError, match="unknown revision"):
        diff_inventory(
            root, root / "data/dependencies/v1", from_rev="does-not-exist"
        )


def test_diff_does_not_open_sockets(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "repo"
    _init(root)
    _inventory(
        root, [repository("acme/tool", (instance(Ecosystem.CARGO, "foo", "1.0.0"),))]
    )
    first = _commit(root, "first")
    _inventory(
        root, [repository("acme/tool", (instance(Ecosystem.CARGO, "foo", "1.1.0"),))]
    )
    second = _commit(root, "second")

    def blocked(*arguments: object, **keywords: object) -> None:
        raise AssertionError("network access is not allowed")

    monkeypatch.setattr(socket.socket, "connect", blocked)
    monkeypatch.setattr(socket, "create_connection", blocked)

    value = diff_inventory(
        root, root / "data/dependencies/v1", from_rev=first, to_rev=second
    )
    assert value.repositories[0].versions[0].direction == "upgrade"
