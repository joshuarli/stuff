from __future__ import annotations

import shutil
from pathlib import Path

import pytest

from repomod.catalog import (
    CatalogError,
    add_repository,
    find_repository,
    load_catalog,
    repository_path,
)

CATALOG_PATH = Path(__file__).resolve().parents[1] / "catalog.toml"


def catalog_text(
    repos: str, *, workspace: str = 'root = "~/repos"\npersonal_owner = "joshuarli"'
) -> str:
    return f'version = 1\n\n[workspace]\n{workspace}\n\n{repos}\n'


def write_catalog(path: Path, repos: str, **kwargs: str) -> Path:
    path.write_text(catalog_text(repos, **kwargs), encoding="utf-8")
    return path


def test_user_catalog_tags_and_workspace_default_are_loaded_without_rewriting(
    tmp_path: Path,
) -> None:
    before = CATALOG_PATH.read_bytes()
    catalog = load_catalog(CATALOG_PATH, {"HOME": str(tmp_path)})
    after = CATALOG_PATH.read_bytes()

    assert before == after
    assert catalog.root == tmp_path / "repos"
    srv = find_repository(catalog, "laputa-systems/srv")
    assert set(srv.tags) == {"tool"}
    assert set(find_repository(catalog, "joshuarli/tansu").tags) == {"app", "rust", "typescript"}
    assert set(find_repository(catalog, "joshuarli/shadowdriver").tags) == {
        "library",
        "private-tool",
        "rust",
        "typescript",
    }
    assert set(find_repository(catalog, "joshuarli/pypi").tags) == {"private-tool", "python"}
    assert set(find_repository(catalog, "joshuarli/toolbox").tags) == {"private-tool", "python"}
    assert set(find_repository(catalog, "joshuarli/pytest-parallel").tags) == {"library", "pypi", "python"}
    assert len(catalog.repositories) > 0


def test_repomod_root_overrides_catalog_root(tmp_path: Path) -> None:
    catalog = load_catalog(
        CATALOG_PATH,
        {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")},
    )

    assert catalog.root == tmp_path / "fleet"


def test_canonical_paths_flatten_personal_owner_and_keep_org_owner(
    tmp_path: Path,
) -> None:
    catalog = load_catalog(
        CATALOG_PATH,
        {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")},
    )

    assert repository_path(catalog, find_repository(catalog, "joshuarli/ish")) == (
        tmp_path / "fleet" / "ish"
    )
    assert repository_path(catalog, find_repository(catalog, "laputa-systems/srv")) == (
        tmp_path / "fleet" / "laputa-systems" / "srv"
    )


def test_bare_name_lookup_rejects_fd_ambiguity() -> None:
    catalog = load_catalog(CATALOG_PATH)

    with pytest.raises(CatalogError, match="ambiguous"):
        find_repository(catalog, "fd")


def test_language_tags_require_a_semantic_role_and_support_multilanguage_repos(tmp_path: Path) -> None:
    path = write_catalog(
        tmp_path / "catalog.toml",
        '[repos."joshuarli"]\ntansu = ["app", "rust", "typescript"]\n'
        'shadowdriver = ["library", "private-tool", "rust", "typescript"]',
    )
    catalog = load_catalog(path, {"HOME": str(tmp_path)})
    assert set(find_repository(catalog, "tansu").tags) == {"app", "rust", "typescript"}
    assert set(find_repository(catalog, "shadowdriver").tags) == {"library", "private-tool", "rust", "typescript"}
    path.write_text(catalog_text('[repos."joshuarli"]\ninvalid = ["rust", "typescript"]'), encoding="utf-8")
    with pytest.raises(CatalogError, match="primary tag"):
        load_catalog(path, {"HOME": str(tmp_path)})


def test_repository_name_casing_is_preserved(tmp_path: Path) -> None:
    path = write_catalog(
        tmp_path / "catalog.toml", '[repos."joshuarli"]\nSingleFile = ["extension"]'
    )

    catalog = load_catalog(path, {"HOME": str(tmp_path)})

    assert catalog.repositories[0].id.name == "SingleFile"


@pytest.mark.parametrize(
    "contents",
    [
        (
            'version = 2\n[workspace]\nroot = "~/repos"\npersonal_owner = "joshuarli"\n'
            '[repos.joshuarli]\na = ["tool"]'
        ),
        (
            'version = 1\n[workspace]\nroot = "~/repos"\npersonal_owner = "joshuarli"\n'
            '[repos.joshuarli]\na = ["rust"]'
        ),
        (
            'version = 1\n[workspace]\nroot = "~/repos"\npersonal_owner = "joshuarli"\n'
            '[repos.joshuarli]\na = ["tool", "tool"]'
        ),
        (
            'version = 1\n[workspace]\nroot = "~/repos"\npersonal_owner = "joshuarli"\n'
            '[repos.joshuarli]\na = "tool"'
        ),
        (
            'version = 1\n[workspace]\nroot = "~/repos"\npersonal_owner = "joshuarli"\n'
            '[repos."../bad"]\na = ["tool"]'
        ),
    ],
)
def test_invalid_catalog_contracts_are_rejected(tmp_path: Path, contents: str) -> None:
    path = tmp_path / "catalog.toml"
    path.write_text(contents, encoding="utf-8")

    with pytest.raises(CatalogError):
        load_catalog(path, {"HOME": str(tmp_path)})


def test_duplicate_owner_and_repository_casing_is_rejected(tmp_path: Path) -> None:
    path = tmp_path / "catalog.toml"
    path.write_text(
        catalog_text(
            '[repos."joshuarli"]\na = ["tool"]\n[repos."JOSHUARLI"]\nb = ["tool"]'
        ),
        encoding="utf-8",
    )
    with pytest.raises(CatalogError, match="duplicate owner"):
        load_catalog(path, {"HOME": str(tmp_path)})
    path.write_text(
        catalog_text(
            '[repos."joshuarli"]\nTool = ["tool"]\ntool = ["library"]'
        ),
        encoding="utf-8",
    )
    with pytest.raises(CatalogError, match="duplicate repository"):
        load_catalog(path, {"HOME": str(tmp_path)})


def test_repository_path_rejects_symlink_components(tmp_path: Path) -> None:
    path = write_catalog(
        tmp_path / "catalog.toml", '[repos."joshuarli"]\nish = ["tool"]'
    )
    root = tmp_path / "repos"
    root.mkdir()
    (root / "ish").symlink_to(tmp_path, target_is_directory=True)
    catalog = load_catalog(
        path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(root)}
    )

    with pytest.raises(CatalogError, match="symlink"):
        repository_path(catalog, catalog.repositories[0])


def test_macos_path_collision_is_rejected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = write_catalog(
        tmp_path / "catalog.toml", '[repos."joshuarli"]\nish = ["tool"]'
    )
    root = tmp_path / "repos"
    root.mkdir()
    (root / "ISH").mkdir()
    monkeypatch.setattr("repomod.catalog.sys.platform", "darwin")
    with pytest.raises(CatalogError, match="collides ignoring case"):
        load_catalog(path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(root)})


def test_add_repository_is_idempotent_and_preserves_catalog_comments(
    tmp_path: Path,
) -> None:
    path = tmp_path / "catalog.toml"
    shutil.copyfile(CATALOG_PATH, path)
    before = path.read_text(encoding="utf-8")
    catalog = load_catalog(
        path,
        {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "repos")},
    )

    assert add_repository(catalog, "new-owner", "new-tool", ["tool"])
    updated = path.read_text(encoding="utf-8")
    assert "# aincrad2 = [\"app\"]" in updated
    assert 'root = "~/repos"' in updated
    assert '[repos."new-owner"]\nnew-tool = ["tool"]' in updated

    updated_catalog = load_catalog(
        path,
        {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "repos")},
    )
    assert not add_repository(updated_catalog, "new-owner", "new-tool", ["tool"])
    assert path.read_text(encoding="utf-8") == updated
    assert updated != before
