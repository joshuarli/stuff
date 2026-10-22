from __future__ import annotations

import json
from pathlib import Path

import pytest

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
    assert (
        "actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # keep v4.3.0"
        in changed_pin
    )
    version_plan = actions._fix(tmp_path, repo, version=True)
    changed_version = version_plan[".github/workflows/ci.yaml"]
    assert "'actions/setup-python@v5' # old version # v5.6.0" in changed_version

    workflow.write_text(changed_pin, encoding="utf-8")
    pin_findings = actions._findings(tmp_path, repo, version=False)
    assert not any(finding.fixable for finding in pin_findings)
    assert actions._fix(tmp_path, repo, version=False) == {}


@pytest.mark.parametrize(
    ("approved", "workflow"),
    [
        ("v4", "uses: actions/checkout@ffffffffffffffffffffffffffffffffffffffff # v7.0.1"),
        ("v7", "uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # v4.4.0"),
    ],
)
def test_action_pin_fixer_refuses_cross_major_changes(
    tmp_path: Path, monkeypatch, approved: str, workflow: str
) -> None:
    pins_path = tmp_path / "actions.toml"
    pins_path.write_text(
        '[actions."actions/checkout"]\n'
        f'channel = "{approved}"\n'
        f'version = "{approved}.0.1"\n'
        'sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(actions, "actions_file", lambda: pins_path)
    root = tmp_path / "repo"
    workflow_path = write_workflow(root, "jobs:\n  test:\n    steps:\n      - " + workflow + "\n")
    repo = repository()

    findings = actions._findings(root, repo, version=False)

    assert len(findings) == 1
    assert not findings[0].fixable
    assert "differs from approved" in findings[0].message
    assert actions._fix(root, repo, version=False) == {}
    assert workflow_path.read_text(encoding="utf-8").endswith(workflow + "\n")


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
    assert f"actions/checkout@{'a' * 40} # v4.3.0" in plan
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


def test_actionlint_is_required_for_workflow_fixer_validation(
    tmp_path: Path, monkeypatch
) -> None:
    monkeypatch.setattr(actions.shutil, "which", lambda _: None)

    errors = actions._validate_workflows(
        tmp_path,
        repository(),
        (".github/workflows/ci.yml",),
    )

    assert errors == [
        "actionlint is required to validate modified GitHub workflows; "
        "install actionlint"
    ]


def test_pin_refresh_keeps_major_channel_unless_explicitly_allowed(
    tmp_path: Path,
    monkeypatch,
) -> None:
    pins_path = tmp_path / "actions.toml"
    write_pins(pins_path)
    monkeypatch.setattr(
        actions,
        "action_tags",
        lambda action, _prefix: (
            ["v4.3.0", "v4.5.0", "v5.0.0"]
            if action == "actions/checkout"
            else ["v5.6.0", "v5.7.0", "v6.0.0"]
        ),
    )
    monkeypatch.setattr(
        actions,
        "resolve_tag",
        lambda _action, tag: "c" * 40 if tag == "v4.5.0" else "d" * 40,
    )

    assert actions.refresh_pins(pins_path)
    pins = actions.load_pins(pins_path)
    assert pins["actions/checkout"].version == "v4.5.0"
    assert pins["actions/checkout"].sha == "c" * 40
    assert not actions.refresh_pins(pins_path)
    assert actions.refresh_pins(pins_path, allow_major=True)
    pins = actions.load_pins(pins_path)
    assert pins["actions/checkout"].channel == "v5"
    assert pins["actions/setup-python"].channel == "v6"


def test_pin_refresh_refuses_to_downgrade_when_upstream_tags_are_incomplete(
    tmp_path: Path, monkeypatch
) -> None:
    pins_path = tmp_path / "actions.toml"
    write_pins(pins_path)
    monkeypatch.setattr(actions, "action_tags", lambda *_: ["v4.3.0"])

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
