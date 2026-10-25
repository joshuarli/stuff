from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from repomod import github
from repomod.rules import actions, release
from repomod.rules.workflows import workflow_files
from repomod.types import RepoId, Repository


def repository(tags: tuple[str, ...] = ("tool",)) -> Repository:
    return Repository(RepoId("acme", "demo"), tags)


def write_pins(path: Path) -> None:
    path.write_text(
        '[actions."actions/checkout"]\nchannel = "v4"\nversion = "v4.4.0"\n'
        'sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"\n\n'
        '[actions."actions/setup-python"]\nchannel = "v5"\nversion = "v5.6.0"\n'
        'sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"\n',
        encoding="utf-8",
    )


def write_workflow(root: Path, contents: str) -> Path:
    workflows = root / ".github" / "workflows"
    workflows.mkdir(parents=True, exist_ok=True)
    path = workflows / "ci.yaml"
    path.write_text(contents, encoding="utf-8")
    return path


def write_cargo_release_script(root: Path) -> None:
    script = root / "tools" / "bump_minor_release.sh"
    script.parent.mkdir(parents=True, exist_ok=True)
    script.write_text(
        "#!/bin/sh\n"
        "cargo owner --list\n"
        "cargo search demo --limit 1\n"
        "cargo publish --dry-run\n"
        "git commit -m release\n"
        "git tag -a demo-v1.2.3\n"
        "cargo publish\n",
        encoding="utf-8",
    )
    script.chmod(0o755)


def test_action_scanner_handles_quoted_refs_comments_blocks_and_local_actions(
    tmp_path: Path,
) -> None:
    workflow = write_workflow(
        tmp_path,
        "jobs:\n"
        "  build:\n"
        "    steps:\n"
        "      - uses: actions/checkout@v4 # keep this\n"
        "      - uses: 'actions/setup-python@v5'\n"
        "      - uses: ./local-action\n"
        "      - uses: docker://alpine:3\n"
        "      - run: |\n"
        "          uses: fake/action@v1\n"
        "      - uses: actions/checkout@${{ env.REF }}\n"
        "      - uses: actions/checkout/subdir@v4\n",
    )

    uses = actions._scan_file(workflow, tmp_path)

    assert len(uses) == 6
    assert uses[0].value == "actions/checkout@v4"
    assert uses[0].comment == "# keep this"
    assert uses[1].quote == "'"
    assert uses[2].action is None
    assert uses[2].error is None
    assert uses[3].action is None
    assert uses[3].error is None
    assert uses[4].action is None
    assert uses[4].error is not None
    assert uses[5].action == "actions/checkout"
    assert uses[5].value == "actions/checkout/subdir@v4"


def test_action_pin_and_version_fixes_preserve_format_and_are_idempotent(
    tmp_path: Path,
    monkeypatch,
) -> None:
    pins_path = tmp_path / "actions.toml"
    write_pins(pins_path)
    monkeypatch.setattr(actions, "actions_file", lambda: pins_path)
    content = (
        "jobs:\n"
        "  build:\n"
        "    steps:\n"
        "      - uses: actions/checkout@v4.3.0 # keep v4.3.0\n"
        "      - uses: 'actions/setup-python@v5' # old version\n"
    )
    workflow = write_workflow(tmp_path, content)
    repo = repository()

    pin_plan = actions._fix(tmp_path, repo, version=False)
    changed_pin = pin_plan[".github/workflows/ci.yaml"]
    assert "actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # keep v4.4.0" in changed_pin
    assert (
        "'actions/setup-python@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'"
        " # old version # v5.6.0"
    ) in changed_pin
    version_plan = actions._fix(tmp_path, repo, version=True)
    changed_version = version_plan[".github/workflows/ci.yaml"]
    assert "'actions/setup-python@v5' # old version # v5.6.0" in changed_version

    workflow.write_text(changed_pin, encoding="utf-8")
    pin_findings = actions._findings(tmp_path, repo, version=False)
    assert not any(finding.fixable for finding in pin_findings)
    assert actions._fix(tmp_path, repo, version=False) == {}


def test_action_pin_fixer_updates_unambiguous_cross_major_reference(
    tmp_path: Path, monkeypatch
) -> None:
    pins_path = tmp_path / "actions.toml"
    pins_path.write_text(
        '[actions."actions/checkout"]\n'
        'channel = "v7"\n'
        'version = "v7.0.1"\n'
        'sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(actions, "actions_file", lambda: pins_path)
    root = tmp_path / "repo"
    workflow_path = write_workflow(
        root,
        "jobs:\n  test:\n    steps:\n"
        "      - uses: actions/checkout@v4.4.0 # v4.4.0\n"
        "      - uses: actions/checkout@cccccccccccccccccccccccccccccccccccccccc # v4.4.0\n",
    )
    repo = repository()

    findings = actions._findings(root, repo, version=False)

    assert len(findings) == 2
    assert all(finding.fixable for finding in findings)
    plan = actions._fix(root, repo, version=False)[".github/workflows/ci.yaml"]
    assert plan.count(f"actions/checkout@{'a' * 40} # v7.0.1") == 2
    version_plan = actions._fix(root, repo, version=True)[".github/workflows/ci.yaml"]
    assert version_plan.count("# v7.0.1") == 2
    assert workflow_path.read_text(encoding="utf-8").endswith(
        "actions/checkout@cccccccccccccccccccccccccccccccccccccccc # v4.4.0\n"
    )


def test_newer_ref_with_stale_comment_is_never_downgraded(
    tmp_path: Path, monkeypatch
) -> None:
    pins_path = tmp_path / "actions.toml"
    pins_path.write_text(
        '[actions."actions/checkout"]\nchannel = "v7"\nversion = "v7.0.1"\n'
        'sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(actions, "actions_file", lambda: pins_path)
    root = tmp_path / "repo"
    source = "uses: actions/checkout@v8 # v7.0.1"
    workflow_path = write_workflow(root, "jobs:\n  test:\n    steps:\n      - " + source + "\n")

    findings = actions._findings(root, repository(), version=False)

    assert len(findings) == 1
    assert not findings[0].fixable
    assert "conflicts with action reference v8" in findings[0].message
    assert actions._fix(root, repository(), version=False) == {}
    assert workflow_path.read_text(encoding="utf-8").endswith(source + "\n")


def test_unknown_action_ref_is_not_made_fixable_by_version_comment(
    tmp_path: Path, monkeypatch
) -> None:
    pins_path = tmp_path / "actions.toml"
    write_pins(pins_path)
    monkeypatch.setattr(actions, "actions_file", lambda: pins_path)
    root = tmp_path / "repo"
    workflow = write_workflow(
        root,
        "jobs:\n  test:\n    steps:\n      - uses: actions/checkout@main # v4.4.0\n",
    )

    findings = actions._findings(root, repository(), version=False)

    assert len(findings) == 1
    assert not findings[0].fixable
    assert "cannot resolve" in findings[0].message
    assert actions._fix(root, repository(), version=False) == {}
    assert "@main # v4.4.0" in workflow.read_text(encoding="utf-8")


def test_stale_unlabelled_sha_fails_closed_and_same_major_pin_is_fixable(
    tmp_path: Path, monkeypatch
) -> None:
    pins_path = tmp_path / "actions.toml"
    write_pins(pins_path)
    monkeypatch.setattr(actions, "actions_file", lambda: pins_path)
    root = tmp_path / "repo"
    workflow_path = write_workflow(
        root,
        "jobs:\n  test:\n    steps:\n"
        "      - uses: actions/checkout@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n"
        "      - uses: actions/checkout@cccccccccccccccccccccccccccccccccccccccc # v4.3.0\n",
    )
    repo = repository()

    findings = actions._findings(root, repo, version=False)
    plan = actions._fix(root, repo, version=False)[".github/workflows/ci.yaml"]

    assert len(findings) == 2
    assert not findings[0].fixable
    assert "cannot identify the major" in findings[0].message
    assert f"actions/checkout@{'a' * 40} # v4.4.0" in plan
    assert "actions/checkout@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n" in plan
    assert workflow_path.read_text(encoding="utf-8").endswith(" # v4.3.0\n")


def test_multiple_version_labels_are_ambiguous_and_never_rewritten(
    tmp_path: Path, monkeypatch
) -> None:
    pins_path = tmp_path / "actions.toml"
    write_pins(pins_path)
    monkeypatch.setattr(actions, "actions_file", lambda: pins_path)
    root = tmp_path / "repo"
    workflow = write_workflow(
        root,
        "jobs:\n  test:\n    steps:\n"
        "      - uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # v4.2.0 from v4.3.0\n",
    )

    findings = actions._findings(root, repository(), version=False)

    assert len(findings) == 1
    assert not findings[0].fixable
    assert "ambiguous" in findings[0].message
    assert actions._fix(root, repository(), version=False) == {}
    assert "# v4.2.0 from v4.3.0" in workflow.read_text(encoding="utf-8")


def test_approved_pins_match_current_repomod_and_binary_release_workflows(
    tmp_path: Path,
) -> None:
    root = Path(__file__).parents[1]
    fixture = tmp_path / "laputa-release"
    workflow = write_workflow(
        fixture,
        "jobs:\n  release:\n    steps:\n"
        "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n"
        "      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2\n",
    )
    current = Repository(RepoId("joshuarli", "repomod"), ("tool", "python"))

    assert not actions._findings(root, current, version=False)
    assert not actions._findings(root, current, version=True)
    assert not actions._findings(fixture, repository(), version=False)
    assert not actions._findings(fixture, repository(), version=True)
    assert workflow.is_file()


def test_action_symlink_parent_and_dynamic_reference_are_reported(
    tmp_path: Path,
) -> None:
    outside = tmp_path / "outside"
    outside.mkdir()
    write_workflow(outside, "jobs: {}\n")
    (tmp_path / ".github").symlink_to(outside / ".github", target_is_directory=True)

    files, problems = workflow_files(tmp_path)

    assert files == []
    assert problems == [(".github", "workflow parent directory is a symlink")]


def test_pinact_is_required_for_workflow_fixer_validation(
    tmp_path: Path, monkeypatch
) -> None:
    monkeypatch.setattr(actions.shutil, "which", lambda _: None)

    errors = actions._validate_workflows(
        tmp_path,
        repository(),
        (".github/workflows/ci.yml",),
    )

    assert errors == [
        "pinact is required to validate modified GitHub Actions files; "
        "install pinact"
    ]


def test_pinact_workflow_validation_only_blocks_errors(
    tmp_path: Path,
    monkeypatch,
) -> None:
    monkeypatch.setattr(actions.shutil, "which", lambda _: "/bin/pinact")
    results = iter(
        [
            SimpleNamespace(
                returncode=1,
                stdout=json.dumps(
                    [
                        {
                            "filepath": ".github/workflows/ci.yml",
                            "line": 3,
                            "column": 9,
                            "id": "pinact-note",
                            "severity": "info",
                            "message": "informational finding",
                            "fixable": False,
                        }
                    ]
                ),
                stderr="",
            ),
            SimpleNamespace(
                returncode=1,
                stdout=json.dumps(
                    [
                        {
                            "filepath": ".github/workflows/ci.yml",
                            "line": 3,
                            "column": 1,
                            "id": "yaml-syntax",
                            "severity": "error",
                            "message": "invalid YAML: bad mapping",
                            "fixable": False,
                        }
                    ]
                ),
                stderr="",
            ),
        ]
    )
    captured: dict[str, Any] = {}

    def fake_run(*args: Any, **kwargs: Any) -> SimpleNamespace:
        captured["args"] = list(args[0])
        captured["cwd"] = kwargs.get("cwd")
        return next(results)

    monkeypatch.setattr(actions.subprocess, "run", fake_run)

    assert actions._validate_workflows(tmp_path, repository(), (".github/workflows/ci.yml",)) == []
    assert actions._validate_workflows(tmp_path, repository(), (".github/workflows/ci.yml",)) == [
        ".github/workflows/ci.yml:3: yaml-syntax: invalid YAML: bad mapping"
    ]
    assert captured["args"][:4] == ["/bin/pinact", "lint", "--profile=correctness", "--format=json"]
    assert captured["args"][4:6] == ["--catalog", str(actions.actions_file())]
    assert "--no-online" not in captured["args"]
    assert captured["cwd"] == tmp_path


def test_workflow_validation_filters_non_workflow_changes(
    tmp_path: Path,
    monkeypatch,
) -> None:
    monkeypatch.setattr(actions.shutil, "which", lambda _: "/bin/pinact")
    captured: dict[str, Any] = {}

    def fake_run(*args: Any, **kwargs: Any) -> SimpleNamespace:
        captured["args"] = list(args[0])
        return SimpleNamespace(returncode=0, stdout="[]", stderr="")

    monkeypatch.setattr(actions.subprocess, "run", fake_run)

    errors = actions._validate_workflows(
        tmp_path,
        repository(),
        (
            ".github/workflows/ci.yml",
            ".github/workflows/actions.lock",
            ".github/dependabot.yml",
            "README.md",
            "action.yaml",
        ),
    )

    assert errors == []
    assert captured["args"][-2:] == [
        str(tmp_path / ".github/workflows/ci.yml"),
        str(tmp_path / "action.yaml"),
    ]


def test_general_actions_fixer_uses_pinact_in_tmp_worktree(
    tmp_path: Path,
    monkeypatch,
) -> None:
    root = tmp_path / "repo"
    root.mkdir()
    workflow = write_workflow(
        root,
        "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n"
        "      - uses: ./local-action\n"
        "      - run: echo \"::set-output name=foo::bar\"\n",
    )

    def git(*arguments: str) -> None:
        subprocess.run(["git", *arguments], cwd=root, check=True,
                       capture_output=True, text=True)

    git("init", "--initial-branch=main")
    git("config", "user.name", "Test User")
    git("config", "user.email", "test@example.invalid")
    git("add", ".github/workflows/ci.yaml")
    git("commit", "-m", "add workflow")

    scratch = tmp_path / "scratch"
    call_path = tmp_path / "pinact-call.json"
    catalog = tmp_path / "actions.toml"
    write_pins(catalog)
    monkeypatch.setattr(actions, "actions_file", lambda: catalog)
    executable = tmp_path / "bin" / "pinact"
    executable.parent.mkdir()
    executable.write_text(
        f"#!{sys.executable}\n"
        "import json, os, sys\n"
        "from pathlib import Path\n"
        "arguments = sys.argv[1:]\n"
        "Path(os.environ['PINACT_CALL']).write_text(json.dumps({\n"
        "    'arguments': arguments, 'cwd': os.getcwd()}), encoding='utf-8')\n"
        "if arguments[0] == 'fix':\n"
        "    assert '--pin' not in arguments\n"
        "    assert arguments[1:3] == ['--catalog', os.environ['PINACT_CATALOG']]\n"
        "    path = Path(arguments[-1])\n"
        "    source = path.read_text(encoding='utf-8')\n"
        "    path.write_text(source.replace(\n"
        "        'echo \"::set-output name=foo::bar\"',\n"
        "        'echo \"foo=bar\" >> \"$GITHUB_OUTPUT\"'), encoding='utf-8')\n"
        "    raise SystemExit(1)\n"
        "if arguments[0] == 'lint':\n"
        "    print('[]')\n"
        "    raise SystemExit(0)\n"
        "raise SystemExit(2)\n",
        encoding="utf-8",
    )
    executable.chmod(0o755)
    monkeypatch.setattr(
        actions.shutil,
        "which",
        lambda name: str(executable) if name == "pinact" else None,
    )
    monkeypatch.setenv("TMPDIR", str(scratch))
    monkeypatch.setenv("PINACT_CALL", str(call_path))
    monkeypatch.setenv("PINACT_CATALOG", str(catalog))

    plan = actions._general_actions_fix(root, repository())

    assert plan == {
        ".github/workflows/ci.yaml": workflow.read_text(encoding="utf-8").replace(
            'echo "::set-output name=foo::bar"',
            'echo "foo=bar" >> "$GITHUB_OUTPUT"',
        )
    }
    call = json.loads(call_path.read_text(encoding="utf-8"))
    assert call["arguments"] == [
        "fix",
        "--catalog",
        str(catalog),
        "--",
        ".github/workflows/ci.yaml",
    ]
    assert Path(call["cwd"]).is_relative_to(scratch)
    assert list(scratch.iterdir()) == []


def test_pinact_lint_reports_rule_ids_with_fixable_and_skips_settings_owned_rules(
    tmp_path: Path,
    monkeypatch,
) -> None:
    workflow = write_workflow(tmp_path, "jobs: {}\n")
    monkeypatch.setattr(actions.shutil, "which", lambda _: "/bin/pinact")
    findings = [
        {
            "filepath": ".github/workflows/ci.yaml",
            "line": 4,
            "column": 9,
            "id": "unpinned-uses",
            "severity": "error",
            "message": "actions/checkout@v4 is not pinned to an immutable commit SHA",
            "fixable": True,
        },
        {
            "filepath": ".github/workflows/ci.yaml",
            "line": 1,
            "column": 1,
            "id": "yaml-syntax",
            "severity": "error",
            "message": "invalid YAML: bad mapping",
            "fixable": False,
        },
        {
            "filepath": ".github/workflows/ci.yaml",
            "line": 2,
            "column": 1,
            "id": "concurrency-limits",
            "severity": "error",
            "message": "owned by gha-settings",
            "fixable": False,
        },
        {
            "filepath": ".github/workflows/ci.yaml",
            "line": 3,
            "column": 5,
            "id": "pinact-note",
            "severity": "info",
            "message": "informational finding",
            "fixable": False,
        },
    ]
    captured: dict[str, Any] = {}

    def fake_run(*args: Any, **kwargs: Any) -> SimpleNamespace:
        captured["args"] = list(args[0])
        captured["cwd"] = kwargs.get("cwd")
        return SimpleNamespace(returncode=1, stdout=json.dumps(findings), stderr="")

    monkeypatch.setattr(actions.subprocess, "run", fake_run)

    results = actions._pinact_lint_findings(tmp_path, repository(), [workflow])

    assert [finding.fixable for finding in results] == [True, False, False]
    assert results[0].message == (
        "line 4: pinact unpinned-uses: "
        "actions/checkout@v4 is not pinned to an immutable commit SHA"
    )
    assert results[1].message == "line 1: pinact yaml-syntax: invalid YAML: bad mapping"
    assert results[2].severity == "info"
    argv = captured["args"]
    assert argv[:4] == ["/bin/pinact", "lint", "--profile=correctness", "--format=json"]
    assert argv[4:6] == ["--catalog", str(actions.actions_file())]
    assert "--no-online" not in argv
    assert captured["cwd"] == tmp_path


def test_pinact_lint_maps_tool_failures_to_operational_findings(
    tmp_path: Path,
    monkeypatch,
) -> None:
    def _fixed_run(case: SimpleNamespace) -> Any:
        def fake_run(*args: Any, **kwargs: Any) -> SimpleNamespace:
            return case

        return fake_run

    workflow = write_workflow(tmp_path, "jobs: {}\n")
    repo = repository()

    monkeypatch.setattr(actions.shutil, "which", lambda _: None)
    assert actions._pinact_lint_findings(tmp_path, repo, [workflow])[0].kind == "operational"

    monkeypatch.setattr(actions.shutil, "which", lambda _: "/bin/pinact")

    def timeout(*args: Any, **kwargs: Any) -> SimpleNamespace:
        raise subprocess.TimeoutExpired(cmd=args[0], timeout=120)

    monkeypatch.setattr(actions.subprocess, "run", timeout)
    findings = actions._pinact_lint_findings(tmp_path, repo, [workflow])
    assert findings[0].kind == "operational"
    assert "timed out" in findings[0].message

    cases = [
        SimpleNamespace(returncode=2, stdout="", stderr="pinact: boom"),
        SimpleNamespace(returncode=1, stdout="not json", stderr=""),
        SimpleNamespace(returncode=1, stdout=json.dumps({"id": "unpinned-uses"}), stderr=""),
        SimpleNamespace(returncode=1, stdout=json.dumps([37]), stderr=""),
        SimpleNamespace(
            returncode=1,
            stdout=json.dumps(
                [
                    {
                        "filepath": "../outside.yml",
                        "line": 1,
                        "column": 1,
                        "id": "unpinned-uses",
                        "severity": "error",
                        "message": "escape",
                        "fixable": True,
                    }
                ]
            ),
            stderr="",
        ),
    ]
    for case in cases:
        monkeypatch.setattr(
            actions.subprocess, "run", _fixed_run(case)
        )
        findings = actions._pinact_lint_findings(tmp_path, repo, [workflow])
        assert len(findings) == 1
        assert findings[0].kind == "operational"


def test_pinact_lint_scopes_to_workflows_and_action_files(
    tmp_path: Path,
    monkeypatch,
) -> None:
    workflow = write_workflow(tmp_path, "jobs: {}\n")
    action_file = tmp_path / "action.yml"
    action_file.write_text("runs: {}\n", encoding="utf-8")
    dependabot = tmp_path / ".github" / "dependabot.yml"
    dependabot.write_text("version: 2\n", encoding="utf-8")
    lock = tmp_path / ".github" / "workflows" / "actions.lock"
    lock.write_text("pinned\n", encoding="utf-8")
    monkeypatch.setattr(actions.shutil, "which", lambda _: "/bin/pinact")
    captured: dict[str, list[str]] = {}

    def fake_run(*args: Any, **kwargs: Any) -> SimpleNamespace:
        captured["args"] = list(args[0])
        return SimpleNamespace(returncode=0, stdout="[]", stderr="")

    monkeypatch.setattr(actions.subprocess, "run", fake_run)

    assert actions._pinact_lint_findings(
        tmp_path, repository(), [workflow, action_file, dependabot, lock]
    ) == []
    assert captured["args"] == [
        "/bin/pinact",
        "lint",
        "--profile=correctness",
        "--format=json",
        "--catalog",
        str(actions.actions_file()),
        "--",
        ".github/workflows/ci.yaml",
        "action.yml",
    ]

    monkeypatch.setattr(
        actions.subprocess,
        "run",
        lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("must not shell out")),
    )
    assert actions._pinact_lint_findings(tmp_path, repository(), [dependabot, lock]) == []


def test_pinact_reports_unpinned_actions_as_fixable(
    tmp_path: Path,
    monkeypatch,
) -> None:
    workflow = write_workflow(
        tmp_path,
        "jobs:\n  test:\n    steps:\n" "      - uses: actions/checkout@v4\n",
    )
    captured: dict[str, list[str]] = {}

    def fake_run(*args: Any, **kwargs: Any) -> SimpleNamespace:
        captured["args"] = list(args[0])
        return SimpleNamespace(
            returncode=1,
            stdout="",
            stderr=".github/workflows/ci.yaml:4: actions/checkout@v4\n",
        )

    monkeypatch.setattr(actions.shutil, "which", lambda _: "/bin/pinact")
    monkeypatch.setattr(actions.subprocess, "run", fake_run)

    findings = actions._pinact_findings(tmp_path, repository(), [workflow])

    assert len(findings) == 1
    assert findings[0].fixable
    assert "line 4" in findings[0].message
    assert "unpinned-action" in findings[0].message
    assert "actions/checkout@v4" in findings[0].message
    argv = captured["args"]
    assert "--format" not in argv
    assert "--no-api" not in argv
    assert "sarif" not in argv


def test_pinact_reports_file_errors_as_unfixable_and_generic_errors_as_operational(
    tmp_path: Path,
    monkeypatch,
) -> None:
    workflow = write_workflow(tmp_path, "jobs: {}\n")
    monkeypatch.setattr(actions.shutil, "which", lambda _: "/bin/pinact")
    monkeypatch.setattr(
        actions.subprocess,
        "run",
        lambda *args, **kwargs: SimpleNamespace(
            returncode=2,
            stdout="",
            stderr="pinact: .github/workflows/ci.yaml:1: unsupported inline uses syntax\n",
        ),
    )

    findings = actions._pinact_findings(tmp_path, repository(), [workflow])

    assert len(findings) == 1
    assert not findings[0].fixable
    assert "line 1" in findings[0].message

    monkeypatch.setattr(
        actions.subprocess,
        "run",
        lambda *args, **kwargs: SimpleNamespace(
            returncode=2,
            stdout="",
            stderr="pinact: cannot resolve actions/checkout: git exited 128\n",
        ),
    )

    findings = actions._pinact_findings(tmp_path, repository(), [workflow])

    assert len(findings) == 1
    assert findings[0].kind == "operational"


def test_pinact_check_clean_returns_no_findings(
    tmp_path: Path,
    monkeypatch,
) -> None:
    workflow = write_workflow(tmp_path, "jobs: {}\n")
    monkeypatch.setattr(actions.shutil, "which", lambda _: "/bin/pinact")
    monkeypatch.setattr(
        actions.subprocess,
        "run",
        lambda *args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout="",
            stderr="",
        ),
    )

    assert actions._pinact_findings(tmp_path, repository(), [workflow]) == []


def test_pin_refresh_selects_latest_major_channel(
    tmp_path: Path,
    monkeypatch,
) -> None:
    pins_path = tmp_path / "actions.toml"
    write_pins(pins_path)
    monkeypatch.setattr(
        actions,
        "action_tags_many",
        lambda repositories, _prefix: {
            action: (
                ("v4.3.0", "v4.5.0", "v5.0.0")
                if action == "actions/checkout"
                else ("v5.6.0", "v5.7.0", "v6.0.0")
            )
            for action in repositories
        },
    )
    monkeypatch.setattr(
        actions,
        "resolve_tags_many",
        lambda tags: {
            action: "c" * 40 if tag == "v4.5.0" else "d" * 40
            for action, tag in tags.items()
        },
    )

    assert actions.refresh_pins(pins_path)
    pins = actions.load_pins(pins_path)
    assert pins["actions/checkout"].version == "v5.0.0"
    assert pins["actions/checkout"].sha == "d" * 40
    assert pins["actions/checkout"].channel == "v5"
    assert pins["actions/setup-python"].version == "v6.0.0"
    assert pins["actions/setup-python"].channel == "v6"
    assert not actions.refresh_pins(pins_path)


def test_pin_refresh_accepts_stable_major_minor_tags(tmp_path: Path, monkeypatch) -> None:
    pins_path = tmp_path / "actions.toml"
    pins_path.write_text(
        '[actions."obi1kenobi/cargo-semver-checks-action"]\n'
        'channel = "v2"\n'
        'version = "v2.8"\n'
        'sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"\n\n'
        '[actions."dtolnay/rust-toolchain"]\n'
        'channel = "v1"\n'
        'version = "v1"\n'
        'sha = "cccccccccccccccccccccccccccccccccccccccc"\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(
        actions,
        "action_tags_many",
        lambda repositories, _prefix: {
            "obi1kenobi/cargo-semver-checks-action": ("v2.8", "v2.10", "v2.9.1"),
            "dtolnay/rust-toolchain": ("v1",),
        },
    )
    monkeypatch.setattr(
        actions,
        "resolve_tags_many",
        lambda tags: dict.fromkeys(tags, "b" * 40),
    )

    assert actions.refresh_pins(pins_path)
    pin = actions.load_pins(pins_path)["obi1kenobi/cargo-semver-checks-action"]
    assert pin.version == "v2.10"
    assert pin.channel == "v2"
    assert actions.load_pins(pins_path)["dtolnay/rust-toolchain"].version == "v1"


def test_pin_refresh_batches_tag_and_commit_reads(tmp_path: Path, monkeypatch) -> None:
    pins_path = tmp_path / "actions.toml"
    write_pins(pins_path)
    queries: list[str] = []

    def graphql(query: str):
        queries.append(query)
        if "refs(first:" in query:
            return {
                "data": {
                    "a0": {
                        "refs": {
                            "nodes": [{"name": "v4.5.0"}],
                            "pageInfo": {"hasNextPage": False, "endCursor": None},
                        }
                    },
                    "a1": {
                        "refs": {
                            "nodes": [{"name": "v5.7.0"}],
                            "pageInfo": {"hasNextPage": False, "endCursor": None},
                        }
                    },
                }
            }
        return {
            "data": {
                "a0": {"ref": {"target": {"__typename": "Commit", "oid": "c" * 40}}},
                "a1": {"ref": {"target": {"__typename": "Commit", "oid": "d" * 40}}},
            }
        }

    monkeypatch.setattr(github, "_graphql", graphql)

    assert actions.refresh_pins(pins_path)
    assert len(queries) == 2
    pins = actions.load_pins(pins_path)
    assert pins["actions/checkout"].sha == "c" * 40
    assert pins["actions/setup-python"].sha == "d" * 40


def test_action_tag_lookup_filters_names_after_querying_tag_namespace(monkeypatch) -> None:
    queries: list[str] = []

    def graphql(query: str):
        queries.append(query)
        return {
            "data": {
                "a0": {
                    "refs": {
                        "nodes": [
                            {"name": "v4.5.0"},
                            {"name": "v5.0.0-rc.1"},
                            {"name": "release/v6.0.0"},
                        ],
                        "pageInfo": {"hasNextPage": False, "endCursor": None},
                    }
                }
            }
        }

    monkeypatch.setattr(github, "_graphql", graphql)

    tags = github.action_tags_many(("actions/checkout",), "v")

    assert tags == {"actions/checkout": ("v4.5.0", "v5.0.0-rc.1")}
    assert 'refPrefix: "refs/tags/"' in queries[0]


def test_pin_refresh_refuses_to_downgrade_when_upstream_tags_are_incomplete(
    tmp_path: Path, monkeypatch
) -> None:
    pins_path = tmp_path / "actions.toml"
    write_pins(pins_path)
    monkeypatch.setattr(
        actions,
        "action_tags_many",
        lambda repositories, _prefix: dict.fromkeys(repositories, ("v4.3.0",)),
    )

    with pytest.raises(ValueError, match="refusing to replace"):
        actions.refresh_pins(pins_path)


def test_cargo_single_crate_metadata_fix_is_safe_and_multi_crate_versions_are_explicit(
    tmp_path: Path,
) -> None:
    single = tmp_path / "single"
    single.mkdir()
    (single / "README.md").write_text("# demo\n", encoding="utf-8")
    (single / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "1.2.3"\n'
        'license = "MIT"\nreadme = "README.md"\n',
        encoding="utf-8",
    )
    write_cargo_release_script(single)
    repo = repository(("library", "crates-io"))

    findings = release._cargo_findings(single, repo)
    plan = release._cargo_fix(single, repo)
    assert any(finding.fixable for finding in findings)
    updated = single / "Cargo.toml"
    updated.write_text(plan["Cargo.toml"], encoding="utf-8")
    assert release._cargo_findings(single, repo) == []

    workspace = tmp_path / "workspace"
    member = workspace / "crates" / "one"
    independent_member = workspace / "crates" / "two"
    member.mkdir(parents=True)
    independent_member.mkdir()
    (workspace / "README.md").write_text("# workspace\n", encoding="utf-8")
    (workspace / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/*"]\n'
        '[workspace.package]\nversion = "2.0.0"\n'
        'license = "MIT"\nrepository = "https://github.com/acme/demo"\n'
        'readme = "README.md"\n',
        encoding="utf-8",
    )
    (member / "Cargo.toml").write_text(
        '[package]\nname = "one"\nversion.workspace = true\nlicense.workspace = true\n'
        'repository.workspace = true\nreadme.workspace = true\n',
        encoding="utf-8",
    )
    (independent_member / "Cargo.toml").write_text(
        '[package]\nname = "two"\nversion = "0.6.0"\nlicense.workspace = true\n'
        'repository.workspace = true\nreadme.workspace = true\n',
        encoding="utf-8",
    )
    internal_member = workspace / "crates" / "internal"
    internal_member.mkdir()
    (internal_member / "Cargo.toml").write_text(
        '[package]\nname = "internal-test-fixture"\npublish = false\n',
        encoding="utf-8",
    )
    write_cargo_release_script(workspace)
    assert release._cargo_findings(workspace, repo) == []


def test_cargo_conflicting_origin_and_outside_readme_need_review(
    tmp_path: Path,
) -> None:
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "1.2.3"\nlicense = "MIT"\n'
        'repository = "https://github.com/someone-else/demo"\n'
        'readme = "../../outside.md"\n',
        encoding="utf-8",
    )

    findings = release._cargo_findings(
        tmp_path, repository(("library", "crates-io"))
    )

    assert any(
        "points outside" in finding.message and not finding.fixable
        for finding in findings
    )
    assert any(
        "README" in finding.message and not finding.fixable for finding in findings
    )


def test_cargo_release_requires_a_reproducible_bump_procedure(tmp_path: Path) -> None:
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "1.2.3"\nlicense = "MIT"\n'
        'repository = "https://github.com/acme/demo"\nreadme = "README.md"\n',
        encoding="utf-8",
    )
    (tmp_path / "README.md").write_text("# demo\n", encoding="utf-8")

    findings = release._cargo_findings(tmp_path, repository(("library", "crates-io")))

    assert any("version-controlled Cargo registry bump procedure" in item.message for item in findings)


def test_unpublished_workspace_inheritance_skips_release_policy_and_fixes(
    tmp_path: Path,
) -> None:
    member = tmp_path / "crates" / "internal"
    member.mkdir(parents=True)
    (tmp_path / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/internal"]\n'
        '[workspace.package]\npublish = false\n',
        encoding="utf-8",
    )
    (member / "Cargo.toml").write_text(
        '[package]\nname = "internal-test"\npublish.workspace = true\n',
        encoding="utf-8",
    )

    registry_library = repository(("library", "crates-io"))
    assert release._cargo_findings(tmp_path, registry_library) == []
    assert release._cargo_fix(tmp_path, registry_library) == {}


def test_pypi_uv_metadata_fix_and_dynamic_version_diagnostic(tmp_path: Path) -> None:
    (tmp_path / "README.md").write_text("# demo\n", encoding="utf-8")
    pyproject = tmp_path / "pyproject.toml"
    pyproject.write_text(
        '[build-system]\nrequires = ["uv_build"]\nbuild-backend = "uv_build"\n\n'
        '[project]\nname = "demo"\nversion = "1.2.3"\n',
        encoding="utf-8",
    )
    repo = repository(("library", "pypi"))

    findings = release._python_findings(tmp_path, repo)
    assert any(finding.fixable for finding in findings)
    pyproject.write_text(
        release._python_fix(tmp_path, repo)["pyproject.toml"], encoding="utf-8"
    )
    assert not any(
        finding.fixable for finding in release._python_findings(tmp_path, repo)
    )

    pyproject.write_text(
        '[build-system]\nbuild-backend = "uv_build"\n\n'
        '[project]\nname = "demo"\ndynamic = ["version"]\n',
        encoding="utf-8",
    )
    assert any(
        "dynamic" in finding.message
        for finding in release._python_findings(tmp_path, repo)
    )


def test_pypi_does_not_require_a_release_workflow(tmp_path: Path) -> None:
    (tmp_path / "pyproject.toml").write_text(
        '[build-system]\nbuild-backend = "uv_build"\n\n'
        '[project]\nname = "demo"\nversion = "1.2.3"\n',
        encoding="utf-8",
    )

    findings = release._python_findings(tmp_path, repository(("library", "pypi")))

    assert not any("release workflow" in finding.message for finding in findings)


def test_npm_lockfile_version_drift_is_detected_and_repaired(tmp_path: Path) -> None:
    package = {"name": "demo", "version": "2.0.0", "packageManager": "npm@10.0.0"}
    lock = {
        "name": "demo",
        "version": "1.0.0",
        "lockfileVersion": 3,
        "packages": {"": {"name": "demo", "version": "1.0.0"}},
    }
    (tmp_path / "package.json").write_text(
        json.dumps(package, indent=2) + "\n", encoding="utf-8"
    )
    lock_path = tmp_path / "package-lock.json"
    lock_path.write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")

    findings = release._npm_findings(tmp_path, repository(("tool", "npm")))
    assert any(finding.fixable for finding in findings)
    lock_path.write_text(
        release._npm_fix(tmp_path, repository())["package-lock.json"],
        encoding="utf-8",
    )
    fixed = json.loads(lock_path.read_text(encoding="utf-8"))
    assert fixed["version"] == "2.0.0"
    assert fixed["packages"][""]["version"] == "2.0.0"
    assert release._npm_findings(tmp_path, repository(("tool", "npm"))) == []


def test_npm_lockfile_without_root_version_is_not_claimed_fixable(
    tmp_path: Path,
) -> None:
    (tmp_path / "package.json").write_text(
        '{"name":"demo","version":"2.0.0"}\n', encoding="utf-8"
    )
    (tmp_path / "package-lock.json").write_text(
        '{"name":"demo","version":"2.0.0","lockfileVersion":3,'
        '"packages":{"":{"name":"demo"}}}\n',
        encoding="utf-8",
    )

    findings = release._npm_findings(tmp_path, repository(("tool", "npm")))

    assert any(
        "root version" in finding.message and not finding.fixable
        for finding in findings
    )
    assert release._npm_fix(tmp_path, repository(("tool", "npm"))) == {}


def test_npm_v1_lockfile_version_drift_can_be_repaired(tmp_path: Path) -> None:
    (tmp_path / "package.json").write_text(
        '{"name":"demo","version":"2.0.0"}\n', encoding="utf-8"
    )
    (tmp_path / "package-lock.json").write_text(
        '{"name":"demo","version":"1.0.0","lockfileVersion":1,"dependencies":{}}\n',
        encoding="utf-8",
    )

    plan = release._npm_fix(tmp_path, repository(("tool", "npm")))

    assert "package-lock.json" in plan
    assert json.loads(plan["package-lock.json"])["version"] == "2.0.0"


def test_private_npm_workspace_checks_public_package_lock_versions(
    tmp_path: Path,
) -> None:
    (tmp_path / "package.json").write_text(
        '{"name":"workspace","private":true,"workspaces":["packages/*",'
        '"!packages/internal"],'
        '"packageManager":"npm@10.0.0"}\n',
        encoding="utf-8",
    )
    package_dir = tmp_path / "packages" / "public"
    package_dir.mkdir(parents=True)
    (package_dir / "package.json").write_text(
        '{"name":"public","version":"1.2.3"}\n', encoding="utf-8"
    )
    internal_dir = tmp_path / "packages" / "internal"
    internal_dir.mkdir(parents=True)
    (internal_dir / "package.json").write_text(
        '{"name":"internal"}\n', encoding="utf-8"
    )
    lock = {
        "name": "workspace",
        "version": "1.0.0",
        "lockfileVersion": 3,
        "packages": {
            "": {"name": "workspace"},
            "packages/public": {"name": "public", "version": "1.2.3"},
        },
    }
    (tmp_path / "package-lock.json").write_text(
        json.dumps(lock) + "\n", encoding="utf-8"
    )

    assert release._npm_findings(tmp_path, repository(("tool", "npm"))) == []

    lock["packages"].pop("packages/public")
    (tmp_path / "package-lock.json").write_text(
        json.dumps(lock) + "\n", encoding="utf-8"
    )
    findings = release._npm_findings(tmp_path, repository(("tool", "npm")))
    assert any("no entry for public workspace package" in item.message for item in findings)


def test_private_npm_package_without_public_workspaces_is_not_published(
    tmp_path: Path,
) -> None:
    (tmp_path / "package.json").write_text(
        '{"name":"internal-tools","private":true}\n', encoding="utf-8"
    )

    assert release._npm_findings(tmp_path, repository(("tool", "npm"))) == []
