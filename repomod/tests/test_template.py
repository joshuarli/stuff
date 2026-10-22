from __future__ import annotations

from pathlib import Path

from repomod.rules.template import _template_findings
from repomod.types import RepoId, Repository


def extension_repository() -> Repository:
    return Repository(RepoId("joshuarli", "extensions"), ("extension", "typescript"))


def test_multi_extension_repository_finds_manifests_inside_project_directories(
    tmp_path: Path,
) -> None:
    (tmp_path / "README.md").write_text("# Browser extensions\n", encoding="utf-8")
    for name in ("extension-allow-right-click", "extension-youtube-transcript"):
        manifest = tmp_path / name / "src" / "manifest.json"
        manifest.parent.mkdir(parents=True)
        manifest.write_text("{}\n", encoding="utf-8")

    assert _template_findings(tmp_path, extension_repository()) == []


def test_extension_manifest_symlinks_are_not_followed(tmp_path: Path) -> None:
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "manifest.json").write_text("{}\n", encoding="utf-8")
    root = tmp_path / "repo"
    root.mkdir()
    (root / "README.md").write_text("# Browser extensions\n", encoding="utf-8")
    (root / "package.json").write_text("{}\n", encoding="utf-8")
    (root / "extension").symlink_to(outside, target_is_directory=True)

    findings = _template_findings(root, extension_repository())

    assert [finding.message for finding in findings] == ["add the browser or editor extension manifest"]
