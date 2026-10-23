from __future__ import annotations

import tomllib
from pathlib import Path

import pytest

from repomod.policy import validate_ignored_rules
from repomod.rules import rules
from repomod.rules.rust_toolchain import (
    _CHANNEL,
    _REQUIRED_COMPONENTS,
    _analysis,
    _findings,
    _fix,
    _read_config,
)
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def apply_plan(root: Path, plan: dict[str, str]) -> None:
    for relative, contents in plan.items():
        (root / relative).write_text(contents, encoding="utf-8")


def test_missing_toolchain_config_is_fixable_for_rust_projects_and_rule_is_blacklistable(
    tmp_path: Path,
) -> None:
    (tmp_path / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
    project = repository("tool")
    finding = _findings(tmp_path, project)
    registered = next(rule for rule in rules() if rule.id == "rust-toolchain")
    ignored = Repository(project.id, project.tags, ignored_rules=frozenset({registered.id}))

    validate_ignored_rules((ignored,))

    assert registered.allowed_paths == ("rust-toolchain.toml",)
    assert [(item.rule, item.file, item.fixable) for item in finding] == [
        ("rust-toolchain", "rust-toolchain.toml", True)
    ]
    plan = _fix(tmp_path, project)
    assert not (tmp_path / "rust-toolchain.toml").exists()
    apply_plan(tmp_path, plan)

    document = tomllib.loads((tmp_path / "rust-toolchain.toml").read_text(encoding="utf-8"))
    assert document == {"toolchain": {"channel": "nightly-2026-09-15", "components": ["rust-src", "clippy"]}}
    assert _findings(tmp_path, project) == []
    assert _fix(tmp_path, project) == plan


def test_outdated_channel_and_components_preserve_extra_components_and_settings(
    tmp_path: Path,
) -> None:
    source = (
        "[toolchain]\n"
        "channel = 'nightly'\n"
        "components = [\n  \"custom-component\",\n]\n"
        "profile = \"minimal\"\n"
        "targets = [\"x86_64-unknown-linux-gnu\"]\n"
        "\n[metadata]\nname = \"demo\"\n"
    )
    path = tmp_path / "rust-toolchain.toml"
    path.write_text(source, encoding="utf-8")
    project = repository("rust")

    findings = _findings(tmp_path, project)
    plan = _fix(tmp_path, project)
    apply_plan(tmp_path, plan)

    document = tomllib.loads(path.read_text(encoding="utf-8"))
    assert len(findings) == 1
    assert findings[0].fixable
    assert document["toolchain"] == {
        "channel": "nightly-2026-09-15",
        "components": ["custom-component", "rust-src", "clippy"],
        "profile": "minimal",
        "targets": ["x86_64-unknown-linux-gnu"],
    }
    assert document["metadata"] == {"name": "demo"}
    assert _findings(tmp_path, project) == []
    assert _fix(tmp_path, project) == plan


def test_missing_toolchain_fields_are_added_without_replacing_other_settings(
    tmp_path: Path,
) -> None:
    source = "[toolchain]\nchannel = 'stable'\nprofile = \"minimal\"\n\n[other]\nvalue = true\n"
    (tmp_path / "rust-toolchain.toml").write_text(source, encoding="utf-8")

    plan = _fix(tmp_path, repository("rust"))
    apply_plan(tmp_path, plan)
    document = tomllib.loads((tmp_path / "rust-toolchain.toml").read_text(encoding="utf-8"))

    assert document == {
        "toolchain": {
            "profile": "minimal",
            "channel": "nightly-2026-09-15",
            "components": ["rust-src", "clippy"],
        },
        "other": {"value": True},
    }


@pytest.mark.parametrize("source", ["[toolchain\n", "toolchain = \"nightly\"\n"])
def test_malformed_or_non_table_toolchain_config_needs_manual_repair(
    tmp_path: Path,
    source: str,
) -> None:
    (tmp_path / "rust-toolchain.toml").write_text(source, encoding="utf-8")
    project = repository("rust")

    findings = _findings(tmp_path, project)
    message, fixable = _analysis(_read_config(tmp_path))

    assert len(findings) == 1
    assert findings[0].fixable is False
    assert message is not None
    assert fixable is False
    with pytest.raises(ValueError, match=r"cannot fix rust-toolchain\.toml"):
        _fix(tmp_path, project)


def test_symlinked_toolchain_config_is_reported_without_following_or_fixing(
    tmp_path: Path,
) -> None:
    target = tmp_path / "target.toml"
    target.write_text('[toolchain]\nchannel = "stable"\n', encoding="utf-8")
    link = tmp_path / "rust-toolchain.toml"
    link.symlink_to(target)
    before = target.read_bytes()
    project = repository("rust")

    findings = _findings(tmp_path, project)

    assert len(findings) == 1
    assert findings[0].message == "rust-toolchain.toml is a symlink"
    assert findings[0].fixable is False
    assert target.read_bytes() == before
    with pytest.raises(ValueError, match=r"cannot fix rust-toolchain\.toml"):
        _fix(tmp_path, project)


def test_non_rust_repository_does_not_receive_toolchain_policy(tmp_path: Path) -> None:
    assert _findings(tmp_path, repository("python")) == []


def test_policy_guide_matches_rust_toolchain_rule_constants() -> None:
    guide = Path(__file__).resolve().parents[1] / "docs" / "policy-development.md"
    contents = guide.read_text(encoding="utf-8")

    assert f"pin `{_CHANNEL}`" in contents
    assert f"include `{_REQUIRED_COMPONENTS[0]}` and `{_REQUIRED_COMPONENTS[1]}`" in contents
