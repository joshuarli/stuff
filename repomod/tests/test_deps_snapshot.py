from __future__ import annotations

import argparse
import json
import socket
from pathlib import Path

import pytest
from git_helpers import git

from repomod.catalog import Catalog, load_catalog
from repomod.cli import _parser
from repomod.cmd import common, deps
from repomod.dependencies.discover import classify, discover
from repomod.dependencies.git_tree import TreeEntry, default_commit, read_blobs, tree_entries
from repomod.dependencies.model import (
    CoverageKind,
    Ecosystem,
    RepositoryState,
    purl,
    redact_url,
)
from repomod.dependencies.snapshot import check, inspect_repository, snapshot
from repomod.types import Repository

DOCUMENT = "version = 1\n\n[workspace]\nroot = \"~/repos\"\npersonal_owner = \"acme\"\n\n"


def _catalog(tmp_path: Path, entries: str) -> Catalog:
    path = tmp_path / "catalog.toml"
    path.write_text(DOCUMENT + entries, encoding="utf-8")
    return load_catalog(path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")})


def _one_repo(tmp_path: Path, name: str = "tool") -> Catalog:
    return _catalog(tmp_path, f'[repos."acme"]\n{name} = ["tool"]\n')


def _repo(catalog: Catalog, name: str) -> Repository:
    return next(repository for repository in catalog.repositories if repository.id.name == name)


def _checkout(
    tmp_path: Path,
    name: str,
    files: dict[str, str | bytes],
    *,
    owner: str = "acme",
) -> Path:
    path = tmp_path / "fleet" / (name if owner == "acme" else f"{owner}/{name}")
    path.mkdir(parents=True)
    git("init", "--initial-branch=main", str(path))
    for relative, content in files.items():
        target = path / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, bytes):
            target.write_bytes(content)
        else:
            target.write_text(content, encoding="utf-8")
    git("add", "-A", cwd=path)
    git("commit", "-m", "initial", cwd=path)
    git("remote", "add", "origin", f"https://github.com/{owner}/{name}.git", cwd=path)
    head = git("rev-parse", "HEAD", cwd=path)
    git("update-ref", "refs/remotes/origin/main", head, cwd=path)
    git("symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main", cwd=path)
    return path


def _purge_origin_head(path: Path) -> None:
    git("symbolic-ref", "--delete", "refs/remotes/origin/HEAD", cwd=path)


def _kinds(repository: Catalog, name: str) -> list[CoverageKind]:
    return [issue.kind for issue in inspect_repository(repository, _repo(repository, name)).data.issues]


def _inventory_files(inventory: Path) -> dict[str, bytes]:
    return {
        path.relative_to(inventory).as_posix(): path.read_bytes()
        for path in sorted(inventory.rglob("*.json"))
    }


def _stats(inventory: Path) -> dict[Path, tuple[int, int]]:
    return {
        path: (path.stat().st_ino, path.stat().st_mtime_ns)
        for path in inventory.rglob("*.json")
    }


def _commit_app_change(path: Path) -> str:
    (path / "README.md").write_text("changed\n", encoding="utf-8")
    git("add", "-A", cwd=path)
    git("commit", "-m", "application change", cwd=path)
    head = git("rev-parse", "HEAD", cwd=path)
    git("update-ref", "refs/remotes/origin/main", head, cwd=path)
    return head


def test_selection_excludes_ignored_and_pending(tmp_path: Path, git_environment: None) -> None:
    catalog = _catalog(
        tmp_path,
        '[repos."acme"]\nactive = ["tool"]\nignored = ["IGNORE", "tool"]\npending = []\n',
    )
    for name in ("active", "ignored", "pending"):
        _checkout(tmp_path, name, {"README.md": f"{name}\n"})

    selected = common.select_repositories(catalog, [], [])
    assert [str(repository.id) for repository in selected] == ["acme/active"]

    inventory = tmp_path / "inventory"
    result = snapshot(catalog, selected, inventory=inventory, filtered=True)

    assert set(result.entries) == {"acme/active"}
    assert sorted(path.name for path in (inventory / "repos" / "acme").iterdir()) == ["active.json"]


def test_reads_origin_head_not_the_checked_out_branch(
    tmp_path: Path, git_environment: None
) -> None:
    path = _checkout(
        tmp_path,
        "tool",
        {"Cargo.toml": "[package]\nname = \"main\"\n", "README.md": "main\n"},
    )
    main_commit = git("rev-parse", "HEAD", cwd=path)
    main_blob = git("rev-parse", "HEAD:Cargo.toml", cwd=path)
    git("checkout", "-b", "feature", cwd=path)
    (path / "Cargo.toml").write_text("[package]\nname = \"feature\"\n", encoding="utf-8")
    git("commit", "-am", "feature", cwd=path)
    feature_commit = git("rev-parse", "HEAD", cwd=path)
    (path / "Cargo.toml").write_text("dirty\n", encoding="utf-8")
    status_before = git("status", "--porcelain", cwd=path)

    catalog = _one_repo(tmp_path)
    inventory = tmp_path / "inventory"
    result = snapshot(catalog, catalog.repositories, inventory=inventory, filtered=True)

    assert result.entries["acme/tool"].commit == main_commit
    assert result.repositories[0].inputs[0].blob == main_blob
    assert git("rev-parse", "HEAD", cwd=path) == feature_commit
    assert git("status", "--porcelain", cwd=path) == status_before


def test_missing_checkout_is_an_unavailable_issue(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = _one_repo(tmp_path)

    inspection = inspect_repository(catalog, catalog.repositories[0])

    assert inspection.commit is None
    assert inspection.data.state is RepositoryState.UNAVAILABLE
    assert [issue.kind for issue in inspection.data.issues] == [CoverageKind.CHECKOUT_MISSING]
    assert inspection.data.inputs == ()


def test_directory_that_is_not_a_work_tree_is_unavailable(
    tmp_path: Path, git_environment: None
) -> None:
    (tmp_path / "fleet" / "tool").mkdir(parents=True)
    catalog = _one_repo(tmp_path)

    assert _kinds(catalog, "tool") == [CoverageKind.CHECKOUT_MISSING]
    inspection = inspect_repository(catalog, catalog.repositories[0])
    assert inspection.data.state is RepositoryState.UNAVAILABLE


def test_origin_mismatch_is_unavailable(tmp_path: Path, git_environment: None) -> None:
    path = _checkout(tmp_path, "tool", {"README.md": "x\n"})
    git("remote", "set-url", "origin", "https://github.com/acme/other.git", cwd=path)
    catalog = _one_repo(tmp_path)

    inspection = inspect_repository(catalog, catalog.repositories[0])

    assert [issue.kind for issue in inspection.data.issues] == [CoverageKind.ORIGIN_MISMATCH]
    assert inspection.data.state is RepositoryState.UNAVAILABLE


def test_missing_origin_head_is_unavailable(tmp_path: Path, git_environment: None) -> None:
    path = _checkout(tmp_path, "tool", {"README.md": "x\n"})
    _purge_origin_head(path)
    catalog = _one_repo(tmp_path)

    inspection = inspect_repository(catalog, catalog.repositories[0])

    assert [issue.kind for issue in inspection.data.issues] == [CoverageKind.DEFAULT_REF_MISSING]
    assert inspection.data.state is RepositoryState.UNAVAILABLE


def test_symlinked_input_is_skipped(tmp_path: Path, git_environment: None) -> None:
    path = _checkout(tmp_path, "tool", {"real.toml": "[package]\n", "README.md": "x\n"})
    (path / "Cargo.toml").symlink_to("real.toml")
    git("add", "Cargo.toml", cwd=path)
    git("commit", "-m", "symlink", cwd=path)
    git("update-ref", "refs/remotes/origin/main", git("rev-parse", "HEAD", cwd=path), cwd=path)
    blob = git("rev-parse", "HEAD:Cargo.toml", cwd=path)
    catalog = _one_repo(tmp_path)

    inspection = inspect_repository(catalog, catalog.repositories[0])

    assert [issue.kind for issue in inspection.data.issues] == [CoverageKind.SYMLINK_SKIPPED]
    assert inspection.data.issues[0].path == "Cargo.toml"
    assert inspection.data.inputs[0].path == "Cargo.toml"
    assert inspection.data.inputs[0].blob == blob
    assert inspection.data.state is RepositoryState.PARTIAL


def test_gitlink_is_reported_as_a_submodule(tmp_path: Path, git_environment: None) -> None:
    path = _checkout(tmp_path, "tool", {"README.md": "x\n"})
    head = git("rev-parse", "HEAD", cwd=path)
    git("update-index", "--add", "--cacheinfo", f"160000,{head},submodule", cwd=path)
    git("commit", "-m", "gitlink", cwd=path)
    git("update-ref", "refs/remotes/origin/main", git("rev-parse", "HEAD", cwd=path), cwd=path)
    catalog = _one_repo(tmp_path)

    inspection = inspect_repository(catalog, catalog.repositories[0])

    assert [issue.kind for issue in inspection.data.issues] == [CoverageKind.SUBMODULE]
    assert inspection.data.issues[0].path == "submodule"
    assert inspection.data.state is RepositoryState.PARTIAL


def test_non_utf8_input_is_reported(tmp_path: Path, git_environment: None) -> None:
    _checkout(tmp_path, "tool", {"Cargo.lock": b"\xff\xfe\x00\n", "README.md": "x\n"})
    catalog = _one_repo(tmp_path)

    inspection = inspect_repository(catalog, catalog.repositories[0])

    assert [issue.kind for issue in inspection.data.issues] == [CoverageKind.NON_UTF8]
    assert inspection.data.issues[0].path == "Cargo.lock"
    assert inspection.data.inputs[0].path == "Cargo.lock"
    assert inspection.data.state is RepositoryState.PARTIAL


def test_git_read_failure_is_a_failed_repository(
    tmp_path: Path, git_environment: None
) -> None:
    path = _checkout(tmp_path, "tool", {"Cargo.toml": "[package]\n", "README.md": "x\n"})
    tree = git("rev-parse", "HEAD^{tree}", cwd=path)
    object_path = path / ".git" / "objects" / tree[:2] / tree[2:]
    assert object_path.is_file()
    object_path.unlink()
    catalog = _one_repo(tmp_path)

    inspection = inspect_repository(catalog, catalog.repositories[0])

    assert [issue.kind for issue in inspection.data.issues] == [CoverageKind.GIT_FAILURE]
    assert inspection.data.state is RepositoryState.FAILED
    assert inspection.commit is not None


def test_vendored_tree_without_manifest_is_reported(
    tmp_path: Path, git_environment: None
) -> None:
    _checkout(
        tmp_path,
        "tool",
        {"vendor/README.chromium": "metadata\n", "README.md": "x\n"},
    )
    catalog = _one_repo(tmp_path)

    inspection = inspect_repository(catalog, catalog.repositories[0])

    assert [issue.kind for issue in inspection.data.issues] == [CoverageKind.VENDORED_UNKNOWN]
    assert inspection.data.issues[0].path == "vendor"


def test_vendored_tree_with_manifest_has_no_vendored_issue(
    tmp_path: Path, git_environment: None
) -> None:
    _checkout(
        tmp_path,
        "tool",
        {"vendor/foo/Cargo.toml": "[package]\nname = \"foo\"\n", "README.md": "x\n"},
    )
    catalog = _one_repo(tmp_path)

    inspection = inspect_repository(catalog, catalog.repositories[0])

    kinds = [issue.kind for issue in inspection.data.issues]
    assert kinds == [CoverageKind.UNSUPPORTED_FORMAT]
    assert inspection.data.issues[0].path == "vendor/foo/Cargo.toml"


def test_recognized_inputs_are_unsupported_until_an_adapter_lands(
    tmp_path: Path, git_environment: None
) -> None:
    files = {
        "Cargo.toml": "[package]\nname = \"tool\"\n",
        "Cargo.lock": "version = 4\n",
        "pyproject.toml": "[project]\nname = \"tool\"\n",
        "uv.lock": "version = 1\n",
        "requirements-dev.txt": "packaging\n",
        "pnpm-lock.yaml": "lockfileVersion: '9.0'\n",
        "package-lock.json": "{\"lockfileVersion\": 3}\n",
        "package.json": "{\"packageManager\": \"pnpm@10.0.0\"}\n",
        "go.mod": "module example.com/tool\n",
        "go.sum": "",
        "Package.swift": "// swift-tools-version: 6.0\n",
        "Package.resolved": "{\"version\": 3}\n",
        "build.zig.zon": ".{\n}\n",
        ".github/workflows/ci.yml": "on: push\n",
        "Dockerfile": "FROM alpine:3.24\n",
        "docker/linux-musl.Dockerfile": "FROM alpine:3.24\n",
        ".gitmodules": "[submodule \"x\"]\n",
        "CMakeLists.txt": "cmake_minimum_required(VERSION 3.30)\n",
        "wrangler.test.jsonc": "{}\n",
        "rust-toolchain.toml": "[toolchain]\nchannel = \"nightly\"\n",
        "Makefile": "all:\n",
        "flake.nix": "{}\n",
    }
    _checkout(tmp_path, "tool", {**files, "README.md": "x\n"})
    catalog = _one_repo(tmp_path)

    inspection = inspect_repository(catalog, catalog.repositories[0])

    assert inspection.data.state is RepositoryState.PARTIAL
    assert inspection.data.projects == ()
    assert inspection.data.edges == ()
    unsupported = [issue for issue in inspection.data.issues if issue.kind is CoverageKind.UNSUPPORTED_FORMAT]
    assert len(unsupported) == len(files)
    assert {issue.path for issue in unsupported} == set(files)


def test_repository_without_inputs_is_complete(
    tmp_path: Path, git_environment: None
) -> None:
    _checkout(tmp_path, "tool", {"README.md": "x\n"})
    catalog = _one_repo(tmp_path)

    inspection = inspect_repository(catalog, catalog.repositories[0])

    assert inspection.data.state is RepositoryState.COMPLETE
    assert inspection.data.issues == ()
    assert inspection.data.inputs == ()
    assert inspection.data.edges == ()


def test_discovery_classifies_real_input_names() -> None:
    assert classify("Cargo.toml") is not None
    assert classify("crates/foo/Cargo.toml") is not None
    assert classify("requirements-dev.txt") is not None
    assert classify(".github/workflows/ci.yml") is not None
    assert classify(".github/workflows/nested/ci.yml") is None
    assert classify("docker/linux-musl.Dockerfile") is not None
    assert classify("vendor/foo/build.gradle") is not None
    assert classify("src/main.rs") is None
    assert classify("README.md") is None

    entries = (
        TreeEntry("Cargo.toml", "a" * 40, "blob"),
        TreeEntry("vendor/README.chromium", "b" * 40, "blob"),
        TreeEntry("Cargo.link", "c" * 40, "symlink"),
        TreeEntry("src/main.rs", "d" * 40, "blob"),
    )
    found = discover(entries)
    assert [item.path for item in found.inputs] == ["Cargo.toml"]
    assert found.symlinks == ()
    assert found.vendored_roots == ("vendor",)
    assert found.vendored_unknown == ("vendor",)


def test_unchanged_files_are_not_rewritten(tmp_path: Path, git_environment: None) -> None:
    catalog = _catalog(tmp_path, '[repos."acme"]\na = ["tool"]\nb = ["tool"]\n')
    _checkout(tmp_path, "a", {"Cargo.toml": "[package]\n", "README.md": "a\n"})
    _checkout(tmp_path, "b", {"README.md": "b\n"})
    inventory = tmp_path / "inventory"

    snapshot(catalog, catalog.repositories, inventory=inventory, filtered=False)
    before = _stats(inventory)
    content = _inventory_files(inventory)

    second = snapshot(catalog, catalog.repositories, inventory=inventory, filtered=False)

    assert second.written == ()
    assert _stats(inventory) == before
    assert _inventory_files(inventory) == content


def test_application_commit_changes_only_the_index(
    tmp_path: Path, git_environment: None
) -> None:
    path = _checkout(tmp_path, "tool", {"Cargo.toml": "[package]\n", "README.md": "a\n"})
    catalog = _one_repo(tmp_path)
    inventory = tmp_path / "inventory"
    snapshot(catalog, catalog.repositories, inventory=inventory, filtered=False)
    repository_file = inventory / "repos" / "acme" / "tool.json"
    before = (repository_file.stat().st_ino, repository_file.stat().st_mtime_ns)
    old_index = json.loads((inventory / "index.json").read_text(encoding="utf-8"))
    head = _commit_app_change(path)

    result = snapshot(catalog, catalog.repositories, inventory=inventory, filtered=False)

    assert result.written == (inventory / "index.json",)
    assert (repository_file.stat().st_ino, repository_file.stat().st_mtime_ns) == before
    index = json.loads((inventory / "index.json").read_text(encoding="utf-8"))
    assert index["repositories"]["acme/tool"]["commit"] == head
    assert index["repositories"]["acme/tool"]["state"] == old_index["repositories"]["acme/tool"]["state"]
    assert (
        index["repositories"]["acme/tool"]["inputs_fingerprint"]
        == old_index["repositories"]["acme/tool"]["inputs_fingerprint"]
    )


def test_filtered_snapshot_preserves_other_repositories(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = _catalog(tmp_path, '[repos."acme"]\na = ["tool"]\nb = ["tool"]\n')
    _checkout(tmp_path, "a", {"README.md": "a\n"})
    b_path = _checkout(tmp_path, "b", {"README.md": "b\n"})
    inventory = tmp_path / "inventory"
    snapshot(catalog, catalog.repositories, inventory=inventory, filtered=False)
    b_file = inventory / "repos" / "acme" / "b.json"
    before = (b_file.stat().st_ino, b_file.stat().st_mtime_ns)
    old_index = json.loads((inventory / "index.json").read_text(encoding="utf-8"))
    new_commit = _commit_app_change(b_path)

    result = snapshot(catalog, [_repo(catalog, "a")], inventory=inventory, filtered=True)

    assert result.written == ()
    assert (b_file.stat().st_ino, b_file.stat().st_mtime_ns) == before
    index = json.loads((inventory / "index.json").read_text(encoding="utf-8"))
    assert index["repositories"]["acme/b"] == old_index["repositories"]["acme/b"]
    assert index["repositories"]["acme/b"]["commit"] != new_commit


def test_unfiltered_snapshot_deletes_dropped_repositories(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = _catalog(
        tmp_path, '[repos."acme"]\na = ["tool"]\n\n[repos."other"]\nb = ["tool"]\n'
    )
    _checkout(tmp_path, "a", {"README.md": "a\n"})
    _checkout(tmp_path, "b", {"README.md": "b\n"}, owner="other")
    inventory = tmp_path / "inventory"
    snapshot(catalog, catalog.repositories, inventory=inventory, filtered=False)
    dropped = inventory / "repos" / "other" / "b.json"
    assert dropped.is_file()

    result = snapshot(catalog, [_repo(catalog, "a")], inventory=inventory, filtered=False)

    assert result.deleted == (dropped,)
    assert not dropped.exists()
    assert not (inventory / "repos" / "other").exists()
    index = json.loads((inventory / "index.json").read_text(encoding="utf-8"))
    assert set(index["repositories"]) == {"acme/a"}


def test_worker_counts_produce_identical_inventories(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = _catalog(
        tmp_path, '[repos."acme"]\na = ["tool"]\nb = ["tool"]\nc = ["tool"]\n'
    )
    _checkout(tmp_path, "a", {"Cargo.toml": "[package]\n", "README.md": "a\n"})
    _checkout(tmp_path, "b", {"pnpm-lock.yaml": "lockfileVersion: '9.0'\n"})
    _checkout(tmp_path, "c", {"Makefile": "all:\n"})
    serial = tmp_path / "serial"
    parallel = tmp_path / "parallel"

    snapshot(catalog, catalog.repositories, inventory=serial, filtered=False, workers=1)
    snapshot(catalog, catalog.repositories, inventory=parallel, filtered=False, workers=8)

    assert _inventory_files(serial) == _inventory_files(parallel)


def test_check_reports_drift_and_writes_nothing(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = _checkout(tmp_path, "tool", {"README.md": "a\n"})
    catalog = _one_repo(tmp_path)
    inventory = tmp_path / "inventory"
    snapshot(catalog, catalog.repositories, inventory=inventory, filtered=False)
    before = _stats(inventory)
    _commit_app_change(path)

    result = check(catalog, catalog.repositories, inventory=inventory, filtered=False)

    assert result.drifted == (inventory / "index.json",)
    assert _stats(inventory) == before

    monkeypatch.setattr(deps, "_inventory", lambda: inventory)
    monkeypatch.setattr(common, "default_catalog", lambda: catalog)
    args = argparse.Namespace(operation="check", repo=[], tag=[], strict=False)
    assert deps._deps(args) == 1


def test_snapshot_cli_reports_strict_coverage(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    catalog = _one_repo(tmp_path)
    inventory = tmp_path / "inventory"
    monkeypatch.setattr(deps, "_inventory", lambda: inventory)
    monkeypatch.setattr(common, "default_catalog", lambda: catalog)

    args = argparse.Namespace(operation="snapshot", repo=[], tag=[], strict=False, workers=1)
    assert deps._deps(args) == 0
    assert (inventory / "index.json").is_file()

    strict = argparse.Namespace(operation="snapshot", repo=[], tag=[], strict=True, workers=1)
    assert deps._deps(strict) == 1


def test_snapshot_and_check_do_not_open_sockets(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    _checkout(tmp_path, "tool", {"Cargo.toml": "[package]\n", "README.md": "a\n"})
    catalog = _one_repo(tmp_path)
    inventory = tmp_path / "inventory"

    def blocked(*arguments: object, **keywords: object) -> None:
        raise AssertionError("network access is not allowed")

    monkeypatch.setattr(socket.socket, "connect", blocked)
    monkeypatch.setattr(socket, "create_connection", blocked)

    snapshot(catalog, catalog.repositories, inventory=inventory, filtered=False)
    assert check(catalog, catalog.repositories, inventory=inventory, filtered=False).drifted == ()


def test_output_is_byte_identical_across_runs(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = _catalog(tmp_path, '[repos."acme"]\na = ["tool"]\nb = ["tool"]\n')
    _checkout(tmp_path, "a", {"Cargo.toml": "[package]\n", "Makefile": "all:\n"})
    _checkout(tmp_path, "b", {"README.md": "b\n"})
    inventory = tmp_path / "inventory"

    snapshot(catalog, catalog.repositories, inventory=inventory, filtered=False)
    first = _inventory_files(inventory)
    snapshot(catalog, catalog.repositories, inventory=inventory, filtered=False)

    assert _inventory_files(inventory) == first
    for content in first.values():
        assert content.endswith(b"\n")


def test_filtered_snapshot_rejects_a_stale_parser_version(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = _one_repo(tmp_path)
    _checkout(tmp_path, "tool", {"README.md": "a\n"})
    inventory = tmp_path / "inventory"
    inventory.mkdir()
    (inventory / "index.json").write_text(
        json.dumps({"schema": 1, "parser_version": 0, "repositories": {}}),
        encoding="utf-8",
    )

    with pytest.raises(ValueError, match="parser version"):
        snapshot(catalog, catalog.repositories, inventory=inventory, filtered=True)


def test_git_tree_reads_the_commit_tree(tmp_path: Path, git_environment: None) -> None:
    path = _checkout(tmp_path, "tool", {"Cargo.toml": "[package]\nname = \"x\"\n"})
    commit = default_commit(path)
    assert commit == git("rev-parse", "HEAD", cwd=path)

    entries = tree_entries(path, commit)
    cargo = next(entry for entry in entries if entry.path == "Cargo.toml")
    assert cargo.kind == "blob"
    assert cargo.object_id == git("rev-parse", "HEAD:Cargo.toml", cwd=path)

    contents = read_blobs(path, [cargo.object_id])
    assert contents[cargo.object_id] == b"[package]\nname = \"x\"\n"


def test_purl_encodes_names_and_qualifiers() -> None:
    assert purl(Ecosystem.CARGO, "serde", "1.0.219") == "pkg:cargo/serde@1.0.219"
    assert purl(Ecosystem.NPM, "@scope/pkg", "1.0.0") == "pkg:npm/%40scope/pkg@1.0.0"
    assert purl(Ecosystem.GENERIC, "cloudflare-d1/binding") == "pkg:generic/cloudflare-d1/binding"
    assert (
        purl(Ecosystem.CARGO, "serde", "1.0.0", qualifiers={"zeta": "z", "alpha": "a b"})
        == "pkg:cargo/serde@1.0.0?alpha=a%20b&zeta=z"
    )
    assert (
        purl(Ecosystem.GITHUB, "actions/checkout", subpath=".github/workflows")
        == "pkg:github/actions/checkout#.github/workflows"
    )
    with pytest.raises(ValueError, match="invalid package name"):
        purl(Ecosystem.CARGO, "")


def test_redact_url_strips_credentials_and_optional_query() -> None:
    assert (
        redact_url("https://user:secret@example.com/path?key=value#fragment")
        == "https://example.com/path?key=value#fragment"
    )
    assert (
        redact_url(
            "https://user:secret@example.com/path?key=value#fragment",
            drop_query_and_fragment=True,
        )
        == "https://example.com/path"
    )
    assert redact_url("git@github.com:acme/tool.git") == "git@github.com:acme/tool.git"


def test_parser_accepts_deps_subcommands() -> None:
    args = _parser().parse_args(["deps", "snapshot", "--repo", "acme/tool", "--workers", "2"])
    assert args.command == "deps"
    assert args.operation == "snapshot"
    assert args.repo == ["acme/tool"]
    assert args.workers == 2
    assert args.strict is False

    args = _parser().parse_args(["deps", "check", "--tag", "tool", "--strict"])
    assert args.operation == "check"
    assert args.tag == ["tool"]
    assert args.strict is True
