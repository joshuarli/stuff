from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from test_binary_release import binary_workflow, write_rust_executable

from repomod.rules import rules
from repomod.rules.binary_release import _binary_release_findings, has_binary_release_contract
from repomod.rules.release_notes import render_workflow_step
from repomod.types import Finding, RepoId, Repository

SHA = "0123456789abcdef" * 4
MAKEFILE = (
    ".PHONY: build verify lint clean\n"
    "build:\n\t./build.sh\n"
    "verify:\n\t./verify.sh\n"
    "lint:\n\t./lint.sh\n"
    "clean:\n\trm -rf out\n"
)


def repository(*tags: str, private: bool = False) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags), private=private)


def check(rule_id: str, root: Path, repo: Repository) -> list[Finding]:
    rule = next(rule for rule in rules() if rule.id == rule_id)
    assert rule.applies_to.matches(repo.tags)
    return rule.check(root, repo)


def release_workflow() -> str:
    notes = "\n".join(render_workflow_step(6))
    return (
        "name: release\n"
        "on:\n"
        "  workflow_dispatch:\n"
        "jobs:\n"
        "  preflight:\n"
        "    runs-on: ubuntu-26.04\n"
        "    steps:\n"
        "      - name: Require the default branch\n"
        "        env:\n"
        "          REF: ${{ github.ref }}\n"
        "        run: |\n"
        "          default_branch=$(gh api \"repos/$REPOSITORY\" --jq '.default_branch')\n"
        "          if [[ \"$REF\" != \"refs/heads/$default_branch\" ]]; then exit 1; fi\n"
        "  release:\n"
        "    needs: preflight\n"
        "    env:\n"
        "      RELEASE_TAG: release-${{ github.sha }}\n"
        "    steps:\n"
        "      - run: |\n"
        "          tar -cJf demo-1.0-x86_64-unknown-linux-musl.tar.xz out\n"
        "          printf '{}' > demo-1.0-x86_64-unknown-linux-musl.manifest.json\n"
        "          sha256sum demo-1.0-* > SHA256SUMS\n"
        "          sha256sum -c SHA256SUMS\n"
        f"{notes}\n"
        "      - run: |\n"
        "          gh release create \"$RELEASE_TAG\" --target \"$GITHUB_SHA\" --prerelease --verify-tag \\\n"
        "            --notes-file \"$RUNNER_TEMP/release-notes.md\" demo-1.0-* SHA256SUMS\n"
    )


def lock_document() -> dict[str, Any]:
    return {
        "schema_version": 1,
        "inputs": [
            {"name": "cpython", "version": "3.15.1", "url": "https://example.test/a.tar.xz", "sha256": SHA, "size": 5},
            {"name": "zlib", "version": "1", "urls": ["https://a.test/z", "https://b.test/z"], "sha256": SHA},
        ],
    }


def write_json(root: Path, name: str, value: object) -> None:
    (root / name).write_text(json.dumps(value), encoding="utf-8")


def build_repo(root: Path) -> None:
    (root / "Makefile").write_text(MAKEFILE, encoding="utf-8")
    write_json(root, "sources.lock.json", lock_document())
    write_json(root, "release-budgets.json", {"schema_version": 1, "targets": {"aarch64-apple-darwin": {}}})
    write_json(root, "bootstrap.lock.json", {"schema_version": 1, "toolchain": "x"})
    write_release(root, release_workflow())


def write_release(root: Path, source: str) -> None:
    workflows = root / ".github" / "workflows"
    workflows.mkdir(parents=True, exist_ok=True)
    (workflows / "release.yml").write_text(source, encoding="utf-8")


BUILD_RULES = ("build-make-targets", "build-locks", "build-release-standard")


def test_compliant_build_repository_passes_every_rule(tmp_path: Path) -> None:
    build_repo(tmp_path)

    for rule_id in BUILD_RULES:
        assert check(rule_id, tmp_path, repository("build", "python")) == []


def test_rules_apply_only_to_build_repositories_and_skip_private_ones(tmp_path: Path) -> None:
    for rule_id in BUILD_RULES:
        rule = next(rule for rule in rules() if rule.id == rule_id)
        assert not rule.applies_to.matches(("tool", "rust"))
        assert rule.check(tmp_path, repository("build", private=True)) == []


def test_make_targets_report_each_missing_target(tmp_path: Path) -> None:
    (tmp_path / "Makefile").write_text("build:\n\ttrue\n.PHONY: verify clean\nlint:\n\ttrue\n", encoding="utf-8")

    findings = check("build-make-targets", tmp_path, repository("build"))

    assert [(finding.file or "").casefold() for finding in findings] == ["makefile", "makefile"]
    assert "verify target" in findings[0].message
    assert "clean target" in findings[1].message


def test_make_targets_require_a_readable_root_makefile(tmp_path: Path) -> None:
    missing = check("build-make-targets", tmp_path, repository("build"))
    assert [(finding.file, finding.kind) for finding in missing] == [("Makefile", "violation")]

    real = tmp_path / "real"
    real.write_text(MAKEFILE, encoding="utf-8")
    (tmp_path / "Makefile").symlink_to(real)
    linked = check("build-make-targets", tmp_path, repository("build"))
    assert [finding.kind for finding in linked] == ["operational"]


def test_make_targets_accept_lowercase_makefile(tmp_path: Path) -> None:
    (tmp_path / "makefile").write_text(MAKEFILE, encoding="utf-8")

    assert check("build-make-targets", tmp_path, repository("build")) == []


def locks_messages(root: Path) -> list[str]:
    return [f"{finding.file}: {finding.message}" for finding in check("build-locks", root, repository("build"))]


def test_missing_sources_lock_is_reported(tmp_path: Path) -> None:
    assert locks_messages(tmp_path) == ["sources.lock.json: build repositories require sources.lock.json"]


def test_optional_lock_files_may_be_absent(tmp_path: Path) -> None:
    write_json(tmp_path, "sources.lock.json", lock_document())

    assert locks_messages(tmp_path) == []


@pytest.mark.parametrize(
    ("mutation", "expected"),
    [
        (lambda d: d.update(schema_version=2), "schema_version must be the integer 1"),
        (lambda d: d.update(schema_version=True), "schema_version must be the integer 1"),
        (lambda d: d.update(inputs=[]), "inputs must be a non-empty list"),
        (lambda d: d.update(inputs={}), "inputs must be a non-empty list"),
        (lambda d: d["inputs"].append("cpython"), "inputs[2] must be an object"),
        (lambda d: d["inputs"][1].update(name="cpython"), "duplicates an earlier input name"),
        (lambda d: d["inputs"][0].update(name=3), "needs a non-empty string name"),
        (lambda d: d["inputs"][0].update(version=3), "needs a string version"),
        (lambda d: d["inputs"][0].update(sha256=SHA.upper()), "sha256 of 64 lowercase hex digits"),
        (lambda d: d["inputs"][0].update(sha256=SHA[:63]), "sha256 of 64 lowercase hex digits"),
        (lambda d: d["inputs"][0].pop("url"), "exactly one of url and urls"),
        (lambda d: d["inputs"][0].update(urls=["https://example.test/b"]), "exactly one of url and urls"),
        (lambda d: d["inputs"][0].update(url=3), "url must be a string"),
        (lambda d: d["inputs"][1].update(urls=[]), "urls must be a non-empty list of strings"),
        (lambda d: d["inputs"][1].update(urls=["ok", 3]), "urls must be a non-empty list of strings"),
        (lambda d: d["inputs"][0].update(size=0), "size must be a positive integer"),
        (lambda d: d["inputs"][0].update(size="5"), "size must be a positive integer"),
        (lambda d: d["inputs"][0].update(size=True), "size must be a positive integer"),
    ],
)
def test_sources_lock_violations(tmp_path: Path, mutation: Any, expected: str) -> None:
    document = lock_document()
    mutation(document)
    write_json(tmp_path, "sources.lock.json", document)

    messages = locks_messages(tmp_path)

    assert len(messages) == 1
    assert messages[0].startswith("sources.lock.json: ")
    assert expected in messages[0]


def test_sources_lock_accepts_extra_fields_and_input_without_size(tmp_path: Path) -> None:
    document = lock_document()
    document["inputs"][0].update(role="source", license="MIT", extra={"a": 1})
    document["other"] = []
    write_json(tmp_path, "sources.lock.json", document)

    assert locks_messages(tmp_path) == []


def test_unparseable_and_non_object_lock_files_are_reported_with_their_path(tmp_path: Path) -> None:
    (tmp_path / "sources.lock.json").write_text("{", encoding="utf-8")
    write_json(tmp_path, "release-budgets.json", [])
    (tmp_path / "bootstrap.lock.json").write_text("nope", encoding="utf-8")

    messages = locks_messages(tmp_path)

    assert [message.split(":")[0] for message in messages] == [
        "sources.lock.json",
        "release-budgets.json",
        "bootstrap.lock.json",
    ]
    assert all("not a valid JSON object" in message for message in messages)


def test_optional_lock_files_need_schema_version_and_budget_targets(tmp_path: Path) -> None:
    write_json(tmp_path, "sources.lock.json", lock_document())
    write_json(tmp_path, "release-budgets.json", {"schema_version": 2, "targets": []})
    write_json(tmp_path, "bootstrap.lock.json", {"toolchain": "x"})

    assert locks_messages(tmp_path) == [
        "release-budgets.json: release-budgets.json schema_version must be the integer 1",
        "release-budgets.json: release-budgets.json needs a targets object",
        "bootstrap.lock.json: bootstrap.lock.json schema_version must be the integer 1",
    ]


def test_symlinked_lock_file_is_operational(tmp_path: Path) -> None:
    real = tmp_path / "real.json"
    real.write_text(json.dumps(lock_document()), encoding="utf-8")
    (tmp_path / "sources.lock.json").symlink_to(real)

    findings = check("build-locks", tmp_path, repository("build"))

    assert [(finding.file, finding.kind) for finding in findings] == [("sources.lock.json", "operational")]


def release_messages(root: Path) -> list[str]:
    return [finding.message for finding in check("build-release-standard", root, repository("build"))]


def test_missing_release_workflow_is_reported(tmp_path: Path) -> None:
    findings = check("build-release-standard", tmp_path, repository("build"))

    assert [(finding.file, finding.kind) for finding in findings] == [
        (".github/workflows/release.yml", "violation")
    ]


def test_release_workflow_symlink_is_operational_only(tmp_path: Path) -> None:
    workflows = tmp_path / ".github" / "workflows"
    workflows.mkdir(parents=True)
    real = tmp_path / "real.yml"
    real.write_text(release_workflow(), encoding="utf-8")
    (workflows / "release.yml").symlink_to(real)

    findings = check("build-release-standard", tmp_path, repository("build"))

    assert [(finding.file, finding.kind) for finding in findings] == [
        (".github/workflows/release.yml", "operational")
    ]


def replaced(old: str, new: str) -> str:
    source = release_workflow()
    assert old in source
    return source.replace(old, new)


@pytest.mark.parametrize(
    ("source", "expected"),
    [
        (replaced("  workflow_dispatch:\n", "  workflow_dispatch:\n  push:\n"), "only on workflow_dispatch"),
        (replaced("on:\n  workflow_dispatch:\n", "on:\n  push:\n"), "only on workflow_dispatch"),
        (replaced("  preflight:\n", "  guard:\n").replace("needs: preflight", ""), "needs a preflight job"),
        (replaced("refs/heads/$default_branch", "main"), "preflight job must compare github.ref"),
        (replaced("REF: ${{ github.ref }}", "REF: x"), "preflight job must compare github.ref"),
        (replaced("release-${{ github.sha }}", "release-${{ github.run_id }}"), "release-${{ github.sha }} tag"),
        (replaced("Write release notes", "Notes"), "shared 'Write release notes' step"),
        (replaced(' --notes-file "$RUNNER_TEMP/release-notes.md" demo', " demo"), "must pass --notes-file"),
        (replaced(" --prerelease", ""), "must pass --prerelease"),
        (replaced(" --verify-tag", ""), "must pass --verify-tag"),
        (replaced(' --target "$GITHUB_SHA"', ""), "must pass --target"),
        (replaced(' --target "$GITHUB_SHA"', " --target main"), "must pass --target"),
        (replaced("SHA256SUMS", "CHECKSUMS"), "SHA256SUMS file"),
        (replaced("sha256sum -c SHA256SUMS", "true"), "sha256sum -c"),
        (replaced(".tar.xz", ".tar.gz"), ".tar.xz archives"),
        (replaced(".manifest.json", ".json"), ".manifest.json manifests"),
    ],
)
def test_release_workflow_violations(tmp_path: Path, source: str, expected: str) -> None:
    write_release(tmp_path, source)

    messages = release_messages(tmp_path)

    assert any(expected in message for message in messages), messages
    assert len(messages) <= 2


def test_release_workflow_accepts_check_long_flag_and_github_sha_expression(tmp_path: Path) -> None:
    source = replaced("sha256sum -c SHA256SUMS", "sha256sum --check SHA256SUMS").replace(
        '--target "$GITHUB_SHA"', "--target ${{ github.sha }}"
    )
    write_release(tmp_path, source)

    assert release_messages(tmp_path) == []


def test_release_workflow_ignores_commented_requirements(tmp_path: Path) -> None:
    write_release(tmp_path, replaced("          sha256sum -c SHA256SUMS\n", "          # sha256sum -c SHA256SUMS\n"))

    assert any("sha256sum -c" in message for message in release_messages(tmp_path))


def test_build_release_is_not_reported_as_a_tool_release(tmp_path: Path) -> None:
    write_rust_executable(tmp_path)
    source = binary_workflow("cargo build --release") + "      - run: ls demo.tar.xz demo.manifest.json\n"
    write_release(tmp_path, source)
    build = repository("build")

    assert _binary_release_findings(tmp_path, build) == []
    assert not has_binary_release_contract(tmp_path, build)
    assert has_binary_release_contract(tmp_path, repository("tool"))
    assert [finding.rule for finding in _binary_release_findings(tmp_path, repository("library"))] == [
        "binary-release-tags"
    ]


def test_build_release_without_the_build_assets_still_needs_the_tool_role(tmp_path: Path) -> None:
    write_rust_executable(tmp_path)
    write_release(tmp_path, binary_workflow("cargo build --release"))

    findings = _binary_release_findings(tmp_path, repository("build"))

    assert [finding.message for finding in findings] == [
        "standard binary prerelease repositories require the tool role"
    ]


def test_tool_rules_do_not_apply_to_build_repositories() -> None:
    for rule_id in ("binary-release-standard", "binary-release-static", "binary-release-notes"):
        rule = next(rule for rule in rules() if rule.id == rule_id)
        assert not rule.applies_to.matches(("build", "python"))
