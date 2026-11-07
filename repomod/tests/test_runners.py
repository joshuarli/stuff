from __future__ import annotations

from pathlib import Path

from repomod.policy import selected_rules
from repomod.rules.runners import (
    _MACOS_POLICY,
    _UBUNTU_POLICY,
    RunnerPolicy,
)
from repomod.types import RepoId, Repository


def repository() -> Repository:
    return Repository(RepoId("acme", "demo"), ("tool",))


def workflow(root: Path, source: str) -> Path:
    path = root / ".github" / "workflows" / "ci.yml"
    path.parent.mkdir(parents=True)
    path.write_text(source, encoding="utf-8")
    return path


def findings(policy: RunnerPolicy, root: Path):
    return policy.findings(root, repository())


def test_version_pinned_macos_runners_are_fixable(tmp_path: Path) -> None:
    source = (
        "jobs:\n"
        "  old:\n"
        "    runs-on: macos-15\n"
        "  latest:\n"
        "    runs-on: macos-latest\n"
        "  matrix:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        os: [macos-13, macos-26, macos-27]\n"
        "  large:\n"
        "    runs-on: macos-15-large\n"
        "  intel:\n"
        "    runs-on: macos-15-intel\n"
        "  accepted:\n"
        "    runs-on: macos-latest-xlarge\n"
    )
    path = workflow(tmp_path, source)

    current = findings(_MACOS_POLICY, tmp_path)

    assert [(finding.line, finding.column) for finding in current] == [
        (3, 14),
        (9, 14),
        (9, 24),
        (9, 34),
        (11, 14),
    ]
    assert all(finding.fixable for finding in current)

    plan = _MACOS_POLICY.fix(tmp_path, repository())

    assert list(plan) == [".github/workflows/ci.yml"]
    repaired = plan[".github/workflows/ci.yml"]
    assert "runs-on: macos-latest\n" in repaired
    assert "[macos-latest, macos-latest, macos-latest]" in repaired
    assert "runs-on: macos-latest-large\n" in repaired
    assert "runs-on: macos-15-intel\n" in repaired
    assert "runs-on: macos-latest-xlarge\n" in repaired
    path.write_text(repaired, encoding="utf-8")

    assert findings(_MACOS_POLICY, tmp_path) == []
    assert _MACOS_POLICY.fix(tmp_path, repository()) == {}


def test_block_list_and_matrix_mapping_macos_runners_are_fixable(tmp_path: Path) -> None:
    source = (
        "jobs:\n"
        "  intel:\n"
        "    runs-on:\n"
        "      - macos-15-intel\n"
        "  matrix:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        include:\n"
        "          - runner: macos-26\n"
        "        os:\n"
        "          - macos-14\n"
        "  xlarge:\n"
        "    runs-on:\n"
        "      - macos-26-xlarge\n"
    )
    workflow(tmp_path, source)

    current = findings(_MACOS_POLICY, tmp_path)

    assert [(finding.line, finding.column) for finding in current] == [
        (9, 21),
        (11, 13),
        (14, 9),
    ]

    repaired = _MACOS_POLICY.fix(tmp_path, repository())[".github/workflows/ci.yml"]

    assert "      - macos-15-intel\n" in repaired
    assert "- runner: macos-latest\n" in repaired
    assert "          - macos-latest\n" in repaired
    assert "- macos-latest-xlarge\n" in repaired


def test_version_pinned_ubuntu_runners_are_fixable(tmp_path: Path) -> None:
    source = (
        "jobs:\n"
        "  pinned:\n"
        "    runs-on: ubuntu-22.04\n"
        "  latest:\n"
        "    runs-on: ubuntu-latest\n"
        "  matrix:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        os: [ubuntu-24.04, ubuntu-26.04, ubuntu-latest-arm]\n"
        "  arm:\n"
        "    runs-on: ubuntu-22.04-arm\n"
        "  old_arm:\n"
        "    runs-on: ubuntu-24.04-arm\n"
        "  custom:\n"
        "    runs-on: ubuntu-24.04-16core\n"
    )
    path = workflow(tmp_path, source)

    current = findings(_UBUNTU_POLICY, tmp_path)

    assert [(finding.line, finding.column) for finding in current] == [
        (3, 14),
        (5, 14),
        (9, 14),
        (9, 42),
        (11, 14),
        (13, 14),
    ]
    assert all(finding.fixable for finding in current)
    assert all("ubuntu-26.04-arm" in finding.message for finding in current)

    plan = _UBUNTU_POLICY.fix(tmp_path, repository())

    assert list(plan) == [".github/workflows/ci.yml"]
    repaired = plan[".github/workflows/ci.yml"]
    assert "runs-on: ubuntu-26.04\n" in repaired
    assert "runs-on: ubuntu-26.04-arm\n" in repaired
    assert "[ubuntu-26.04, ubuntu-26.04, ubuntu-26.04-arm]" in repaired
    assert "runs-on: ubuntu-24.04-16core\n" in repaired
    path.write_text(repaired, encoding="utf-8")

    assert findings(_UBUNTU_POLICY, tmp_path) == []
    assert _UBUNTU_POLICY.fix(tmp_path, repository()) == {}


def test_runner_rules_have_autofixes() -> None:
    rules = {
        rule.id: rule
        for rule in selected_rules(rule_ids=("gha-macos", "gha-ubuntu"))
    }

    assert set(rules) == {"gha-macos", "gha-ubuntu"}
    assert rules["gha-macos"].commit_message == "chore(repomod): update macos runners"
    assert rules["gha-ubuntu"].commit_message == "chore(repomod): update ubuntu runners"
    for rule in rules.values():
        assert rule.fix is not None
        assert rule.allowed_paths == (
            ".github/workflows/*.yml",
            ".github/workflows/*.yaml",
        )


def test_comments_and_non_runner_mentions_are_not_reported(tmp_path: Path) -> None:
    workflow(
        tmp_path,
        "# runs-on: macos-14\n"
        "jobs:\n"
        "  test:\n"
        "    name: build on macos-15\n"
        "    runs-on: ubuntu-26.04-arm\n"
        "    steps:\n"
        "      - name: build on macos-15\n"
        "      - name: test on ubuntu-22.04\n"
        "      - 'build macos-14'\n"
        "      - uses: actions/checkout@v4 # macos-14\n",
    )

    assert findings(_MACOS_POLICY, tmp_path) == []
    assert findings(_UBUNTU_POLICY, tmp_path) == []


def test_symlinked_workflow_reports_an_operational_finding(tmp_path: Path) -> None:
    workflows = tmp_path / ".github" / "workflows"
    workflows.mkdir(parents=True)
    outside = tmp_path.parent / "outside-ci.yml"
    outside.write_text("jobs: {}\n", encoding="utf-8")
    (workflows / "ci.yml").symlink_to(outside)

    current = findings(_MACOS_POLICY, tmp_path)

    assert len(current) == 1
    assert current[0].kind == "operational"
