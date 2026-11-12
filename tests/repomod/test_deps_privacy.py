from __future__ import annotations

import json
from dataclasses import asdict
from pathlib import Path

import pytest
from git_helpers import git, make_catalog

from repomod.catalog import repository_path
from repomod.dependencies.adapters.javascript import JavaScriptAdapter
from repomod.dependencies.adapters.zig import ZigAdapter
from repomod.dependencies.model import InputContent
from repomod.dependencies.snapshot import inspect_repository, repository_dict, serialize

URL = "https://example-user:example-secret@example.com/pkg.tgz?token=example-query#example-fragment"
PRIVATE = ("example-user", "example-secret", "example-query", "example-fragment")


def npm_lock() -> str:
    return json.dumps({"lockfileVersion": 3, "packages": {
        "": {"dependencies": {"demo": URL}},
        "node_modules/demo": {"version": "1.0.0", "resolved": URL},
    }})


def pnpm_lock() -> str:
    return ("lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      demo:\n"
            f"        specifier: {URL}\n        version: {URL}\n"
            f"packages:\n  demo@1.0.0:\n    resolution: {{tarball: '{URL}'}}\n")


def zon_lock() -> str:
    return '.{ .dependencies = .{ .demo = .{ .url = "' + URL + '" } } }'


@pytest.mark.parametrize("kind", ["npm", "pnpm", "zig", "pnpm-error"])
def test_complete_adapter_outputs_keep_url_secrets_private(kind: str) -> None:
    if kind == "zig":
        result = ZigAdapter().extract((InputContent("build.zig.zon", zon_lock()),))
    else:
        filename = "package-lock.json" if kind == "npm" else "pnpm-lock.yaml"
        text = npm_lock() if kind == "npm" else pnpm_lock()
        if kind == "pnpm-error":
            text = f"broken: [{URL}\n\n  malformed: : ]"
        result = JavaScriptAdapter(registry=URL).extract((InputContent(filename, text),))
    serialized = json.dumps(asdict(result), default=str)
    for secret in PRIVATE:
        assert secret not in serialized
    if kind == "pnpm-error":
        assert result.issues
    else:
        assert result.instances
        assert any("example.com" in instance.purl for instance in result.instances) or kind == "npm"
        assert all(edge.constraint == "https://example.com/pkg.tgz" for edge in result.edges)


def test_tracked_inventory_redacts_all_dependency_url_representations(
    tmp_path: Path, git_environment: None,
) -> None:
    catalog, repository = make_catalog(tmp_path)
    checkout = repository_path(catalog, repository)
    checkout.mkdir(parents=True)
    git("init", "--initial-branch=main", str(checkout))
    (checkout / "package-lock.json").write_text(npm_lock())
    (checkout / "build.zig.zon").write_text(zon_lock())
    git("add", ".", cwd=checkout)
    git("commit", "-m", "fixture", cwd=checkout)
    git("remote", "add", "origin", "https://github.com/acme/tool.git", cwd=checkout)
    git("update-ref", "refs/remotes/origin/main", "HEAD", cwd=checkout)
    git("symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main", cwd=checkout)
    data = inspect_repository(catalog, repository).data
    serialized = serialize(repository_dict(data))
    assert data.instances
    assert data.edges
    for secret in PRIVATE:
        assert secret not in serialized
