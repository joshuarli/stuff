from __future__ import annotations

import argparse
import json
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

import repomod.cli as cli_module
from repomod import github
from repomod.catalog import CatalogError, add_pending, add_repository, load_catalog
from repomod.cmd.discover import _discover
from repomod.cmd.topics import _topics


def catalog(path: Path, text: str) -> Path:
    path.write_text(text, encoding="utf-8")
    return path


def test_discover_owner_lists_only_active_owned_repositories(monkeypatch) -> None:
    def gh_json(arguments: list[str], input_text: str | None = None):
        if arguments == ["users/acme"]:
            return {"type": "User"}
        if arguments == ["user"]:
            return {"login": "someone-else"}
        assert arguments == ["--paginate", "--slurp", "users/acme/repos?type=all&per_page=100"]
        return [
            [
                {"full_name": "acme/keep", "archived": False, "owner": {"login": "acme"}},
                {"full_name": "acme/old", "archived": True, "owner": {"login": "acme"}},
                {"full_name": "other/fork", "archived": False, "owner": {"login": "other"}},
                {"full_name": "acme/broken", "archived": False, "owner": {}},
                {"archived": False, "owner": {"login": "acme"}},
            ]
        ]

    monkeypatch.setattr(github, "_gh_json", gh_json)

    assert github.discover(("acme",)) == ["acme/keep"]


def test_discover_owner_includes_private_repositories_for_authenticated_user(
    monkeypatch,
) -> None:
    def gh_json(arguments: list[str], input_text: str | None = None):
        if arguments == ["users/acme"]:
            return {"type": "User"}
        if arguments == ["user"]:
            return {"login": "acme"}
        if arguments == ["--paginate", "--slurp", "users/acme/repos?type=all&per_page=100"]:
            return [[{"full_name": "acme/public", "archived": False, "owner": {"login": "acme"}}]]
        assert arguments == ["--paginate", "--slurp", "user/repos?type=all&per_page=100"]
        return [
            [
                {"full_name": "acme/public", "archived": False, "owner": {"login": "acme"}},
                {"full_name": "acme/private", "archived": False, "owner": {"login": "acme"}},
                {"full_name": "acme/old", "archived": True, "owner": {"login": "acme"}},
            ]
        ]

    monkeypatch.setattr(github, "_gh_json", gh_json)

    assert github.discover(("acme",)) == ["acme/private", "acme/public"]


def test_discover_sorts_and_deduplicates_across_owners(monkeypatch) -> None:
    def gh_json(arguments: list[str], input_text: str | None = None):
        if arguments == ["user"]:
            return {"login": "nobody"}
        if len(arguments) == 1 and arguments[0].startswith("users/"):
            return {"type": "User"}
        endpoint = arguments[-1]
        owner = endpoint.split("/")[1]
        if owner == "beta":
            return [[{"full_name": "beta/tool", "archived": False, "owner": {"login": "beta"}}]]
        return [[{"full_name": "ACME/tool", "archived": False, "owner": {"login": "acme"}}]]

    monkeypatch.setattr(github, "_gh_json", gh_json)

    assert github.discover(("acme", "beta")) == ["ACME/tool", "beta/tool"]


def test_discover_cli_reports_uncatalogued_and_missing(tmp_path: Path, monkeypatch, capsys) -> None:
    path = catalog(
        tmp_path / "catalog.toml",
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\nkeep = ["tool"]\ngone = ["tool"]\n',
    )
    catalog_value = load_catalog(path, {"HOME": str(tmp_path)})
    monkeypatch.setattr("repomod.cmd.common.default_catalog", lambda: catalog_value)
    monkeypatch.setattr(
        "repomod.github.discover", lambda owners: ["acme/keep", "acme/new"]
    )
    args = argparse.Namespace(owner=[], json=False)

    assert _discover(args) == 0
    out = capsys.readouterr().out.splitlines()
    assert out[0] == "added to pending (1):"
    assert out[1] == "  acme/new"
    assert out[2:6] == ["pending review (1):", "  acme/new", "missing from GitHub (1):", "  acme/gone"]


def test_discover_cli_matches_exactly_and_supports_json(tmp_path: Path, monkeypatch, capsys) -> None:
    path = catalog(
        tmp_path / "catalog.toml",
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\nkeep = ["tool"]\n',
    )
    catalog_value = load_catalog(path, {"HOME": str(tmp_path)})
    monkeypatch.setattr("repomod.cmd.common.default_catalog", lambda: catalog_value)
    monkeypatch.setattr("repomod.github.discover", lambda owners: ["ACME/keep"])
    args = argparse.Namespace(owner=[], json=False)

    assert _discover(args) == 0
    assert capsys.readouterr().out.splitlines() == [
        "added to pending (0): none",
        "catalog matches GitHub",
    ]

    args = argparse.Namespace(owner=["acme"], json=True)
    assert _discover(args) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload == {"owners": ["acme"], "uncatalogued": [], "missing": [], "pending": []}


def test_discover_cli_validates_and_deduplicates_owners(tmp_path: Path, monkeypatch) -> None:
    path = catalog(
        tmp_path / "catalog.toml",
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\nkeep = ["tool"]\n',
    )
    monkeypatch.setattr(
        "repomod.cmd.common.default_catalog", lambda: load_catalog(path, {"HOME": str(tmp_path)})
    )
    seen: list[tuple[str, ...]] = []
    monkeypatch.setattr(
        "repomod.github.discover",
        lambda owners: seen.append(owners) or [],
    )

    assert _discover(argparse.Namespace(owner=["beta", "BETA", "acme"], json=True)) == 0
    assert seen == [("acme", "beta")]

    with pytest.raises(CatalogError, match="invalid owner"):
        _discover(argparse.Namespace(owner=["../bad"], json=False))


def test_discover_is_registered_in_cli_parser() -> None:
    args = cli_module._parser().parse_args(["discover", "--owner", "acme", "--json"])

    assert args.command == "discover"
    assert args.owner == ["acme"]
    assert args.json is True


def test_discover_missing_is_limited_to_requested_owners(tmp_path: Path, monkeypatch, capsys) -> None:
    path = catalog(
        tmp_path / "catalog.toml",
        'version = 1\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n'
        '[repos."acme"]\nkeep = ["tool"]\n[repos."beta"]\nother = ["tool"]\n',
    )
    monkeypatch.setattr("repomod.cmd.common.default_catalog", lambda: load_catalog(path))
    monkeypatch.setattr("repomod.github.discover", lambda owners: ["acme/keep"])
    assert _discover(argparse.Namespace(owner=["acme"], json=True)) == 0
    assert json.loads(capsys.readouterr().out)["missing"] == []


def test_discover_records_new_repositories_and_skips_ignored(tmp_path: Path, monkeypatch, capsys) -> None:
    path = catalog(
        tmp_path / "catalog.toml",
        'version = 3\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n'
        '[repos."acme"]\nkeep = ["tool"]\nold = ["IGNORE", "tool"]\n',
    )
    monkeypatch.setattr("repomod.cmd.common.default_catalog", lambda: load_catalog(path))
    monkeypatch.setattr("repomod.github.discover", lambda owners: ["ACME/keep", "acme/OLD", "acme/new"])
    args = argparse.Namespace(owner=[], json=True)
    assert _discover(args) == 0
    assert json.loads(capsys.readouterr().out)["uncatalogued"] == ["acme/new"]
    updated = load_catalog(path)
    assert [str(repo.id) for repo in updated.repositories] == ["acme/keep"]
    assert [str(repo) for repo in updated.pending] == ["acme/new"]
    assert _discover(args) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["uncatalogued"] == []
    assert payload["pending"] == ["acme/new"]


def test_catalog_rejects_repository_in_multiple_states(tmp_path: Path) -> None:
    path = catalog(
        tmp_path / "catalog.toml",
        'version = 3\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n'
        '[repos."acme"]\nkeep = []\n[repos."ACME"]\nKEEP = ["IGNORE"]\n',
    )
    with pytest.raises(CatalogError, match="duplicate owner"):
        load_catalog(path)


def test_tags_promote_pending_repository_in_place(tmp_path: Path) -> None:
    path = catalog(
        tmp_path / "catalog.toml",
        'version = 3\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n'
        '[repos."acme"]\nkeep = []\n',
    )
    pending = load_catalog(path)
    assert [str(repo) for repo in pending.pending] == ["acme/keep"]
    assert pending.repositories == ()
    path.write_text(path.read_text().replace('keep = []', 'keep = ["tool"]'))
    promoted = load_catalog(path)
    assert promoted.pending == ()
    assert [(str(repo.id), repo.tags) for repo in promoted.repositories] == [("acme/keep", ("tool",))]


def test_ignored_repositories_do_not_reach_topics(tmp_path: Path, monkeypatch) -> None:
    path = catalog(
        tmp_path / "catalog.toml",
        'version = 3\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n'
        '[repos."acme"]\nkeep = ["tool"]\nold = ["IGNORE", "tool"]\nother = ["IGNORE"]\n',
    )
    catalog_value = load_catalog(path)
    monkeypatch.setattr("repomod.cmd.common.default_catalog", lambda: catalog_value)
    seen = []
    monkeypatch.setattr("repomod.cmd.topics.topics_many", lambda repos, **kwargs: seen.extend(repos) or [])
    assert _topics(argparse.Namespace(repo=[], tag=[], operation="apply", workers=1)) == 0
    assert [(str(repo.id), repo.tags) for repo in seen] == [("acme/keep", ("tool",))]
    assert {str(repo.id) for repo in catalog_value.ignored} == {"acme/old", "acme/other"}


def test_concurrent_discovery_keeps_both_updates_and_can_promote(tmp_path: Path) -> None:
    path = catalog(
        tmp_path / "catalog.toml",
        'version = 3\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n'
        '[repos."acme"]\nkeep = ["tool"]\n',
    )
    current = load_catalog(path)
    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(add_pending, current, [name]) for name in ("acme/one", "acme/two")]
        for future in futures:
            future.result()
    current = load_catalog(path)
    assert {str(repo) for repo in current.pending} == {"acme/one", "acme/two"}
    assert add_repository(current, "acme", "one", ["tool"])
    current = load_catalog(path)
    assert {str(repo) for repo in current.pending} == {"acme/two"}
    assert {str(repo.id) for repo in current.repositories} == {"acme/keep", "acme/one"}
