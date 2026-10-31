from __future__ import annotations

from pathlib import Path

import pytest

from repomod.catalog import CatalogError, load_catalog
from repomod.policy import PolicyError, selected_rules, validate_ignored_rules
from repomod.rules import registry_sources
from repomod.rules.registry_sources import (
    RegistryPolicy,
    load_registry_policy,
    parse_registry_policy,
    registry_source_findings,
)
from repomod.types import RepoId, Repository

CARGO = "sparse+https://crates.example/current/crates-io/"
NPM = "https://npm.example/current/"
PYPI = "https://pypi.example/current/simple/"
POLICY = RegistryPolicy(True, CARGO, NPM, PYPI)
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"


def repository(*tags: str, ignored: tuple[str, ...] = ()) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags), ignored_rules=frozenset(ignored))


def write(root: Path, relative: str, text: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def messages(root: Path, repo: Repository | None = None) -> list[tuple[str | None, str]]:
    return [
        (item.file, item.message)
        for item in registry_source_findings(root, repo or repository(), POLICY)
    ]


def cargo_config(source: str = CARGO) -> str:
    return (
        '[source.crates-io]\nreplace-with = "stuff"\n\n'
        f'[source.stuff]\nregistry = "{source}"\n'
    )


def cargo_lock(*packages: tuple[str, str | None]) -> str:
    blocks = []
    for name, source in packages:
        line = f'source = "{source}"\n' if source is not None else ""
        blocks.append(f'[[package]]\nname = "{name}"\nversion = "1.0.0"\n{line}')
    return "version = 4\n\n" + "\n".join(blocks)


def test_cargo_replacement_with_crates_io_lock_ids_is_clean(tmp_path: Path) -> None:
    write(tmp_path, ".cargo/config.toml", cargo_config())
    write(tmp_path, "Cargo.lock", cargo_lock(("serde", CRATES_IO), ("demo", None)))

    assert messages(tmp_path) == []


def test_cargo_missing_or_wrong_replacement_is_flagged(tmp_path: Path) -> None:
    write(tmp_path, "Cargo.lock", cargo_lock(("serde", CRATES_IO)))
    [(file, message)] = messages(tmp_path)
    assert file == ".cargo/config.toml"
    assert "no .cargo/config.toml found" in message
    assert CARGO in message

    write(tmp_path, ".cargo/config.toml", cargo_config("sparse+https://index.crates.io/"))
    [(file, message)] = messages(tmp_path)
    assert file == ".cargo/config.toml"
    assert "does not replace crates-io" in message


def test_cargo_alternate_registry_and_git_sources(tmp_path: Path) -> None:
    write(tmp_path, ".cargo/config.toml", cargo_config())
    write(
        tmp_path,
        "Cargo.lock",
        cargo_lock(
            ("a", "registry+https://other.example/index"),
            ("b", "registry+https://other.example/index"),
            ("fork", "git+https://github.com/acme/fork?rev=abc#abc"),
            ("vendored", "git+https://github.com/acme/vendored#def"),
        ),
    )

    found = messages(tmp_path, repository(ignored=("registry-sources:git:vendored",)))

    assert len(found) == 2
    assert any("2 cargo package(s) resolve from registry+https://other.example/index" in m for _, m in found)
    assert any(
        "cargo package fork resolves from git source" in m and "registry-sources:git:fork" in m
        for _, m in found
    )


def test_cargo_config_in_ancestor_covers_nested_lockfile(tmp_path: Path) -> None:
    write(tmp_path, ".cargo/config.toml", cargo_config())
    write(tmp_path, "tools/Cargo.lock", cargo_lock(("serde", CRATES_IO)))

    assert messages(tmp_path) == []


def test_npm_registry_config_and_lock(tmp_path: Path) -> None:
    write(tmp_path, ".npmrc", f"registry={NPM}\n")
    write(tmp_path, "package.json", "{}")
    write(
        tmp_path,
        "pnpm-lock.yaml",
        "lockfileVersion: '9.0'\n\nimporters:\n\n  .:\n    dependencies:\n      shared:\n"
        "        specifier: link:../outside\n        version: link:../outside\n\n"
        f"packages:\n\n  left@1.0.0:\n    resolution: {{tarball: {NPM}left/-/left-1.0.0.tgz}}\n\n"
        "  '@scope/right@2.0.0':\n"
        "    resolution: {tarball: https://registry.npmjs.org/@scope/right/-/right-2.0.0.tgz}\n\n"
        "  forked@1.0.0:\n"
        "    resolution: {tarball: https://codeload.github.com/acme/forked/tar.gz/abc}\n",
    )

    found = messages(tmp_path)

    assert len(found) == 3
    assert any("registry.npmjs.org (@scope/right)" in m for _, m in found)
    assert any("npm package forked resolves from git source" in m for _, m in found)
    assert any("links ../outside outside the repository" in m for _, m in found)


def test_npm_workspace_links_inside_repository_are_allowed(tmp_path: Path) -> None:
    write(tmp_path, ".npmrc", f"registry={NPM}\n")
    write(
        tmp_path,
        "pnpm-lock.yaml",
        "importers:\n\n  packages/app:\n    dependencies:\n      lib:\n"
        "        specifier: workspace:*\n        version: link:../lib\n",
    )

    assert messages(tmp_path) == []


def test_npmrc_missing_foreign_or_scoped_registry_and_workspace_override(tmp_path: Path) -> None:
    write(tmp_path, "pnpm-lock.yaml", "lockfileVersion: '9.0'\n")
    [(file, message)] = messages(tmp_path)
    assert file == ".npmrc"
    assert f"registry={NPM}" in message

    write(
        tmp_path,
        ".npmrc",
        f"; comment\nregistry={NPM}\n@acme:registry=https://registry.npmjs.org/\n",
    )
    write(tmp_path, "pnpm-workspace.yaml", "registry: https://registry.npmjs.org/\npackages: []\n")
    found = messages(tmp_path)
    assert sorted(file or "" for file, _ in found) == [".npmrc", "pnpm-workspace.yaml"]


def test_package_lock_resolved_urls(tmp_path: Path) -> None:
    write(tmp_path, ".npmrc", f"registry={NPM}\n")
    write(
        tmp_path,
        "package-lock.json",
        '{"lockfileVersion": 3, "packages": {"": {}, '
        f'"node_modules/ok": {{"resolved": "{NPM}ok/-/ok-1.0.0.tgz"}}, '
        '"node_modules/bad": {"resolved": "https://registry.npmjs.org/bad/-/bad-1.0.0.tgz"}, '
        '"node_modules/local": {"resolved": "packages/local", "link": true}}}',
    )

    [(file, message)] = messages(tmp_path)

    assert file == "package-lock.json"
    assert "registry.npmjs.org (bad)" in message


def test_python_index_and_lock(tmp_path: Path) -> None:
    write(
        tmp_path,
        "pyproject.toml",
        f'[project]\nname = "demo"\n\n[[tool.uv.index]]\nurl = "{PYPI}"\ndefault = true\n',
    )
    write(
        tmp_path,
        "uv.lock",
        "version = 1\n\n"
        f'[[package]]\nname = "good"\nsource = {{ registry = "{PYPI}" }}\n\n'
        '[[package]]\nname = "demo"\nsource = { editable = "." }\n\n'
        '[[package]]\nname = "bad"\nsource = { registry = "https://pypi.org/simple" }\n\n'
        '[[package]]\nname = "gitdep"\nsource = { git = "https://github.com/acme/gitdep#abc" }\n\n'
        '[[package]]\nname = "outside"\nsource = { directory = "../elsewhere" }\n',
    )

    found = messages(tmp_path)

    assert len(found) == 3
    assert all(file == "uv.lock" for file, _ in found)
    assert any("https://pypi.org/simple (bad)" in m for _, m in found)
    assert any("python package gitdep resolves from git source" in m for _, m in found)
    assert any("outside the repository" in m for _, m in found)


def test_python_without_default_index_is_flagged(tmp_path: Path) -> None:
    write(tmp_path, "pyproject.toml", '[project]\nname = "demo"\n')

    [(file, message)] = messages(tmp_path)

    assert file == "pyproject.toml"
    assert PYPI in message

    write(tmp_path, "uv.toml", f'[[index]]\nurl = "{PYPI}"\ndefault = true\n')
    assert messages(tmp_path) == []


def test_fork_repositories_and_repositories_without_lockfiles_are_skipped(tmp_path: Path) -> None:
    write(tmp_path, "Cargo.lock", cargo_lock(("serde", CRATES_IO)))
    assert messages(tmp_path, repository("fork")) == []
    (tmp_path / "Cargo.lock").unlink()
    assert messages(tmp_path) == []


def test_rule_is_disabled_until_policy_enforces(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    write(tmp_path, "Cargo.lock", cargo_lock(("serde", CRATES_IO)))
    policy_file = tmp_path / "registries.toml"
    registry_sources._default_policy.cache_clear()
    monkeypatch.setattr(registry_sources, "registry_policy_file", lambda: policy_file)

    policy_file.write_text(policy_source(enforce=False), encoding="utf-8")
    assert registry_sources._check(tmp_path, repository()) == []

    registry_sources._default_policy.cache_clear()
    policy_file.write_text(policy_source(enforce=True), encoding="utf-8")
    assert [item.rule for item in registry_sources._check(tmp_path, repository())] == [
        "registry-sources"
    ]
    registry_sources._default_policy.cache_clear()


def policy_source(*, enforce: bool) -> str:
    return (
        f"enforce = {str(enforce).lower()}\n\n[cargo]\nsource = \"{CARGO}\"\n\n"
        f'[npm]\nregistry = "{NPM}"\n\n[python]\nindex = "{PYPI}"\n'
    )


def test_registered_rule_is_selectable() -> None:
    assert [rule.id for rule in selected_rules(rule_ids=["registry-sources"])] == ["registry-sources"]


def test_shipped_policy_is_valid_and_disabled() -> None:
    policy = load_registry_policy()
    assert policy.cargo_source.startswith("sparse+https://crates.")
    assert policy.enforce is False


@pytest.mark.parametrize(
    "mutation",
    [
        lambda s: s.replace("enforce = true", "enforce = 1"),
        lambda s: s.replace(NPM, "http://npm.example/current/"),
        lambda s: s.replace(PYPI, PYPI.rstrip("/")),
        lambda s: s.replace(CARGO, "https://crates.example/"),
        lambda s: s + '\n[extra]\nx = "y"\n',
        lambda s: s.replace("[python]\n", "[python]\nmirror = \"x\"\n"),
        lambda s: "enforce = true\n",
    ],
)
def test_policy_is_validated_strictly(mutation) -> None:
    with pytest.raises(ValueError, match=r"registries\.toml"):
        parse_registry_policy(mutation(policy_source(enforce=True)))


def catalog_with(tmp_path: Path, ignore: str) -> Path:
    path = tmp_path / "catalog.toml"
    path.write_text(
        'version = 3\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["library", "rust"]\n\n'
        f'[overrides."acme/demo"]\nignore = ["{ignore}"]\nignore_reason = "vendored fork"\n',
        encoding="utf-8",
    )
    return path


def test_git_source_ignore_is_accepted_by_catalog_and_policy(tmp_path: Path) -> None:
    catalog = load_catalog(
        catalog_with(tmp_path, "registry-sources:git:h2-futures-lite"), {"HOME": str(tmp_path)}
    )
    validate_ignored_rules(catalog.repositories)


@pytest.mark.parametrize("code", ["registry-sources:git:", "registry-sources:git:a b", "registry-sources:git"])
def test_malformed_git_source_ignore_is_rejected(tmp_path: Path, code: str) -> None:
    path = catalog_with(tmp_path, code)
    with pytest.raises((CatalogError, PolicyError)):
        validate_ignored_rules(load_catalog(path, {"HOME": str(tmp_path)}).repositories)


def apply(root: Path, plan: dict[str, str]) -> None:
    for relative, text in plan.items():
        write(root, relative, text)


def test_plan_configures_every_ecosystem_and_clears_the_fixable_findings(tmp_path: Path) -> None:
    write(tmp_path, "Cargo.lock", "version = 4\n")
    write(tmp_path, ".cargo/config.toml", "[build]\njobs = 2\n")
    write(tmp_path, "web/package.json", "{}\n")
    write(tmp_path, "web/pnpm-lock.yaml", "lockfileVersion: '9.0'\n")
    write(tmp_path, "web/.npmrc", "save-exact=true")
    write(
        tmp_path,
        "pyproject.toml",
        '[project]\nname = "demo"\nversion = "1"\n\n[tool.uv]\ndev-dependencies = []\n',
    )
    write(tmp_path, "uv.lock", "version = 1\n")

    plan = registry_sources.registry_source_plan(tmp_path, repository(), POLICY, relock=False)
    apply(tmp_path, plan)

    assert sorted(plan) == [".cargo/config.toml", "pyproject.toml", "web/.npmrc"]
    assert "[build]\njobs = 2" in plan[".cargo/config.toml"]
    assert plan["web/.npmrc"] == f"save-exact=true\nregistry={NPM}\n"
    assert 'environments = ["sys_platform == \'darwin\'", "sys_platform == \'linux\'"]' in plan["pyproject.toml"]
    assert messages(tmp_path) == []


def test_plan_refuses_to_overwrite_a_different_registry_or_cargo_source(tmp_path: Path) -> None:
    write(tmp_path, "Cargo.lock", "version = 4\n")
    write(tmp_path, ".cargo/config.toml", '[source.crates-io]\nreplace-with = "mirror"\n')
    with pytest.raises(RuntimeError, match="already defines"):
        registry_sources.registry_source_plan(tmp_path, repository(), POLICY, relock=False)

    other = tmp_path / "other"
    write(other, "package.json", "{}\n")
    write(other, ".npmrc", "registry=https://registry.npmjs.org/\n")
    with pytest.raises(RuntimeError, match="replace it"):
        registry_sources.registry_source_plan(other, repository(), POLICY, relock=False)


def test_path_source_outside_the_repository_is_ignored_by_name(tmp_path: Path) -> None:
    write(
        tmp_path,
        "uv.lock",
        'version = 1\n\n[[package]]\nname = "private"\nversion = "1"\n'
        'source = { directory = "../private" }\n',
    )
    write(tmp_path, "pyproject.toml", f'[[tool.uv.index]]\nurl = "{PYPI}"\ndefault = true\n')

    assert any("outside the repository" in m and "registry-sources:path:private" in m for _, m in messages(tmp_path))
    assert messages(tmp_path, repository(ignored=("registry-sources:path:private",))) == []


def test_path_source_ignore_is_accepted_by_catalog_and_policy(tmp_path: Path) -> None:
    catalog = load_catalog(catalog_with(tmp_path, "registry-sources:path:shadowdriver"), {"HOME": str(tmp_path)})
    validate_ignored_rules(catalog.repositories)
