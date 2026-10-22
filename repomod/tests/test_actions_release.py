from __future__ import annotations

import json
from pathlib import Path

from repomod.rules import actions, release
from repomod.types import Repository, RepoId


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
    assert uses[2].action is None and uses[2].error is None
    assert uses[3].action is None and uses[3].error is None
    assert uses[4].action is None and uses[4].error is not None
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
        "      - uses: actions/checkout@v3 # important\n"
        "      - uses: 'actions/setup-python@v5' # old version\n"
    )
    workflow = write_workflow(tmp_path, content)
    repo = repository()

    pin_plan = actions._fix(tmp_path, repo, version=False)
    changed_pin = pin_plan[".github/workflows/ci.yaml"]
    assert "actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # important" in changed_pin
    version_plan = actions._fix(tmp_path, repo, version=True)
    changed_version = version_plan[".github/workflows/ci.yaml"]
    assert "'actions/setup-python@v5' # old version # v5.6.0" in changed_version

    workflow.write_text(changed_pin, encoding="utf-8")
    pin_findings = actions._findings(tmp_path, repo, version=False)
    assert not any(finding.fixable for finding in pin_findings)
    assert actions._fix(tmp_path, repo, version=False) == {}


def test_action_symlink_parent_and_dynamic_reference_are_reported(tmp_path: Path) -> None:
    outside = tmp_path / "outside"
    outside.mkdir()
    write_workflow(outside, "jobs: {}\n")
    (tmp_path / ".github").symlink_to(outside / ".github", target_is_directory=True)

    files, problems = actions._workflow_files(tmp_path)

    assert files == []
    assert problems == [(".github", "workflow parent directory is a symlink")]


def test_actionlint_is_required_for_workflow_fixer_validation(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(actions.shutil, "which", lambda _: None)

    errors = actions._validate_workflows(
        tmp_path,
        repository(),
        (".github/workflows/ci.yml",),
    )

    assert errors == ["actionlint is required to validate modified GitHub workflows; install actionlint"]


def test_pin_refresh_keeps_major_channel_unless_explicitly_allowed(
    tmp_path: Path,
    monkeypatch,
) -> None:
    pins_path = tmp_path / "actions.toml"
    write_pins(pins_path)
    monkeypatch.setattr(actions, "action_tags", lambda _action, _prefix: ["v4.3.0", "v4.5.0", "v5.0.0"])
    monkeypatch.setattr(actions, "resolve_tag", lambda _action, tag: "c" * 40 if tag == "v4.5.0" else "d" * 40)

    assert actions.refresh_pins(pins_path)
    pins = actions.load_pins(pins_path)
    assert pins["actions/checkout"].version == "v4.5.0"
    assert pins["actions/checkout"].sha == "c" * 40
    assert not actions.refresh_pins(pins_path)


def test_cargo_single_crate_metadata_fix_is_safe_and_multi_crate_versions_are_explicit(
    tmp_path: Path,
) -> None:
    single = tmp_path / "single"
    single.mkdir()
    (single / "README.md").write_text("# demo\n", encoding="utf-8")
    (single / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "1.2.3"\nlicense = "MIT"\nreadme = "README.md"\n',
        encoding="utf-8",
    )
    repo = repository(("library", "crates-io"))

    findings = release._cargo_findings(single, repo)
    plan = release._cargo_fix(single, repo)
    assert any(finding.fixable for finding in findings)
    updated = single / "Cargo.toml"
    updated.write_text(plan["Cargo.toml"], encoding="utf-8")
    assert release._cargo_findings(single, repo) == []

    workspace = tmp_path / "workspace"
    member = workspace / "crates" / "one"
    member.mkdir(parents=True)
    (workspace / "README.md").write_text("# workspace\n", encoding="utf-8")
    (workspace / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/*"]\n[workspace.package]\nversion = "2.0.0"\n'
        'license = "MIT"\nrepository = "https://github.com/acme/demo"\nreadme = "README.md"\n',
        encoding="utf-8",
    )
    (member / "Cargo.toml").write_text(
        '[package]\nname = "one"\nversion.workspace = true\nlicense.workspace = true\n'
        'repository.workspace = true\nreadme.workspace = true\n',
        encoding="utf-8",
    )
    assert release._cargo_findings(workspace, repo) == []


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
    pyproject.write_text(release._python_fix(tmp_path, repo)["pyproject.toml"], encoding="utf-8")
    assert not any(finding.fixable for finding in release._python_findings(tmp_path, repo))

    pyproject.write_text(
        '[build-system]\nbuild-backend = "uv_build"\n\n'
        '[project]\nname = "demo"\ndynamic = ["version"]\n',
        encoding="utf-8",
    )
    assert any("dynamic" in finding.message for finding in release._python_findings(tmp_path, repo))


def test_npm_lockfile_version_drift_is_detected_and_repaired(tmp_path: Path) -> None:
    package = {"name": "demo", "version": "2.0.0", "packageManager": "npm@10.0.0"}
    lock = {
        "name": "demo",
        "version": "1.0.0",
        "lockfileVersion": 3,
        "packages": {"": {"name": "demo", "version": "1.0.0"}},
    }
    (tmp_path / "package.json").write_text(json.dumps(package, indent=2) + "\n", encoding="utf-8")
    lock_path = tmp_path / "package-lock.json"
    lock_path.write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")

    findings = release._npm_findings(tmp_path, repository(("tool", "npm")))
    assert any(finding.fixable for finding in findings)
    lock_path.write_text(release._npm_fix(tmp_path, repository())["package-lock.json"], encoding="utf-8")
    fixed = json.loads(lock_path.read_text(encoding="utf-8"))
    assert fixed["version"] == "2.0.0"
    assert fixed["packages"][""]["version"] == "2.0.0"
    assert release._npm_findings(tmp_path, repository(("tool", "npm"))) == []
