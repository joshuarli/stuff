from __future__ import annotations

from pathlib import Path

from repomod.policy import selected_rules
from repomod.rules.macos_runners import _fix_macos_runners, _macos_runner_findings
from repomod.types import RepoId, Repository


def repository() -> Repository:
    return Repository(RepoId("acme", "demo"), ("tool",))


def workflow(root: Path, source: str) -> Path:
    path = root / ".github" / "workflows" / "ci.yml"
    path.parent.mkdir(parents=True)
    path.write_text(source, encoding="utf-8")
    return path


def test_old_and_floating_macos_runners_are_fixable(tmp_path: Path) -> None:
    source = (
        "jobs:\n"
        "  test:\n"
        "    runs-on: macos-15\n"
        "  latest:\n"
        "    runs-on: macos-latest\n"
        "  matrix:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        os: [macos-13, macos-26, macos-27]\n"
        "  large:\n"
        "    runs-on: macos-14-large\n"
        "  accepted:\n"
        "    runs-on: macos-27-xlarge\n"
    )
    path = workflow(tmp_path, source)

    findings = _macos_runner_findings(tmp_path, repository())

    assert [(finding.file, finding.line, finding.column) for finding in findings] == [
        (".github/workflows/ci.yml", 3, 14),
        (".github/workflows/ci.yml", 5, 14),
        (".github/workflows/ci.yml", 9, 14),
        (".github/workflows/ci.yml", 11, 14),
    ]
    assert all(finding.fixable for finding in findings)

    plan = _fix_macos_runners(tmp_path, repository())

    assert list(plan) == [".github/workflows/ci.yml"]
    repaired = plan[".github/workflows/ci.yml"]
    assert "runs-on: macos-26\n" in repaired
    assert "runs-on: macos-latest" not in repaired
    assert "[macos-26, macos-26, macos-27]" in repaired
    assert "runs-on: macos-26-large\n" in repaired
    assert "runs-on: macos-27-xlarge\n" in repaired
    path.write_text(repaired, encoding="utf-8")

    assert _macos_runner_findings(tmp_path, repository()) == []
    assert _fix_macos_runners(tmp_path, repository()) == {}


def test_macos_runner_rule_has_an_autofix() -> None:
    rule = next(rule for rule in selected_rules(rule_ids=("gha-macos",)))

    assert rule.fix is not None
    assert rule.commit_message == "chore(repomod): update macos runners"
    assert rule.allowed_paths == (".github/workflows/*.yml", ".github/workflows/*.yaml")


def test_comments_and_non_runner_mentions_are_not_reported(tmp_path: Path) -> None:
    workflow(
        tmp_path,
        "# runs-on: macos-14\n"
        "jobs:\n"
        "  test:\n"
        "    name: build on macos-15\n"
        "    runs-on: ubuntu-latest\n",
    )

    assert _macos_runner_findings(tmp_path, repository()) == []


def test_symlinked_workflow_reports_an_operational_finding(tmp_path: Path) -> None:
    workflows = tmp_path / ".github" / "workflows"
    workflows.mkdir(parents=True)
    outside = tmp_path.parent / "outside-ci.yml"
    outside.write_text("jobs: {}\n", encoding="utf-8")
    (workflows / "ci.yml").symlink_to(outside)

    findings = _macos_runner_findings(tmp_path, repository())

    assert len(findings) == 1
    assert findings[0].kind == "operational"
