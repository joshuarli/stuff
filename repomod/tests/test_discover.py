from __future__ import annotations

import argparse
import json
from pathlib import Path

import pytest

import repomod.cli as cli_module
from repomod import github
from repomod.catalog import CatalogError, load_catalog
from repomod.cmd.discover import _discover


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
    monkeypatch.setattr("repomod.cmd.common._catalog", lambda: catalog_value)
    monkeypatch.setattr(
        "repomod.github.discover", lambda owners: ["acme/keep", "acme/new"]
    )
    args = argparse.Namespace(owner=[], json=False)

    assert _discover(args) == 0
    out = capsys.readouterr().out.splitlines()
    assert out[0] == "uncatalogued (1):"
    assert out[1] == "  acme/new"
    assert out[2] == "missing from GitHub (1):"
    assert out[3] == "  acme/gone"


def test_discover_cli_matches_exactly_and_supports_json(tmp_path: Path, monkeypatch, capsys) -> None:
    path = catalog(
        tmp_path / "catalog.toml",
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\nkeep = ["tool"]\n',
    )
    catalog_value = load_catalog(path, {"HOME": str(tmp_path)})
    monkeypatch.setattr("repomod.cmd.common._catalog", lambda: catalog_value)
    monkeypatch.setattr("repomod.github.discover", lambda owners: ["ACME/keep"])
    args = argparse.Namespace(owner=[], json=False)

    assert _discover(args) == 0
    assert capsys.readouterr().out.splitlines() == [
        "uncatalogued (0): none",
        "catalog matches GitHub",
    ]

    args = argparse.Namespace(owner=["acme"], json=True)
    assert _discover(args) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload == {"owners": ["acme"], "uncatalogued": [], "missing": []}


def test_discover_cli_validates_and_deduplicates_owners(tmp_path: Path, monkeypatch) -> None:
    path = catalog(
        tmp_path / "catalog.toml",
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\nkeep = ["tool"]\n',
    )
    monkeypatch.setattr(
        "repomod.cmd.common._catalog", lambda: load_catalog(path, {"HOME": str(tmp_path)})
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
