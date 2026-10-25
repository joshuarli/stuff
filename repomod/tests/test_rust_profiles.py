from __future__ import annotations

from pathlib import Path

from repomod.rules.rust_profiles import _check as check_profiles
from repomod.rules.rust_profiles import _fix as fix_profiles
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def write_manifest(root: Path, body: str) -> None:
    (root / "Cargo.toml").write_text(body, encoding="utf-8")


def thin_release() -> str:
    return (
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
        "[profile.release]\nopt-level = 3\nlto = \"thin\"\npanic = \"abort\"\nstrip = false\n"
    )


def test_absent_profiles_are_clean(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write_manifest(root, "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n")
    assert check_profiles(root, repository("tool", "rust")) == []


def test_forks_are_skipped(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write_manifest(
        root,
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n"
        "[profile.release]\nopt-level = 3\nlto = \"fat\"\ncodegen-units = 1\n"
        "panic = \"abort\"\nstrip = true\n",
    )
    assert check_profiles(root, repository("tool", "rust", "fork")) == []


def test_fat_release_is_fixable(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write_manifest(
        root,
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
        "[profile.release]\nopt-level = 3\nlto = \"fat\"\ncodegen-units = 1\n"
        "panic = \"abort\"\nstrip = true\n",
    )
    findings = check_profiles(root, repository("tool", "rust"))
    assert len(findings) == 1
    assert findings[0].rule == "rust-release-profiles"
    assert findings[0].fixable

    plan = fix_profiles(root, repository("tool", "rust"))
    assert set(plan) == {"Cargo.toml"}
    (root / "Cargo.toml").write_text(plan["Cargo.toml"], encoding="utf-8")
    assert check_profiles(root, repository("tool", "rust")) == []


def test_fixer_appends_missing_release_table(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write_manifest(root, "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n")
    assert check_profiles(root, repository("tool", "rust")) == []
    plan = fix_profiles(root, repository("tool", "rust"))
    assert plan == {}


def test_missing_dist_for_binary_contract_is_fixable(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    (root / "src").mkdir()
    (root / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
    write_manifest(root, thin_release())
    workflows = root / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "release.yml").write_text(
        "name: release\n"
        "on:\n"
        "  workflow_dispatch:\n"
        "jobs:\n"
        "  build:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        target: [x86_64-unknown-linux-musl, aarch64-apple-darwin]\n"
        "    steps:\n"
        "      - run: cargo build --profile dist --target \"${{ matrix.target }}\"\n"
        "      - uses: actions/upload-artifact@v7\n"
        "      - uses: actions/download-artifact@v7\n"
        "      - run: zip dist/demo.zip demo\n"
        "  release:\n"
        "    env:\n"
        "      RELEASE_TAG: release-${{ github.sha }}\n"
        "    steps:\n"
        "      - run: gh release create \"$RELEASE_TAG\" dist/demo.zip --prerelease --target \"$GITHUB_SHA\"\n",
        encoding="utf-8",
    )
    findings = check_profiles(root, repository("tool", "rust"))
    assert [finding.message for finding in findings if finding.fixable] != []
    assert all(finding.rule == "rust-release-profiles" for finding in findings)

    plan = fix_profiles(root, repository("tool", "rust"))
    assert set(plan) == {"Cargo.toml"}
    (root / "Cargo.toml").write_text(plan["Cargo.toml"], encoding="utf-8")
    assert [finding for finding in check_profiles(root, repository("tool", "rust")) if finding.fixable] == []


def test_bare_release_in_prerelease_workflow_is_manual(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    (root / "src").mkdir()
    (root / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
    write_manifest(root, thin_release())
    workflows = root / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "release.yml").write_text(
        "name: release\n"
        "on:\n"
        "  workflow_dispatch:\n"
        "jobs:\n"
        "  build:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        target: [x86_64-unknown-linux-musl, aarch64-apple-darwin]\n"
        "    steps:\n"
        "      - run: cargo build --locked --release --target \"${{ matrix.target }}\"\n"
        "      - uses: actions/upload-artifact@v7\n"
        "      - uses: actions/download-artifact@v7\n"
        "      - run: zip dist/demo.zip demo\n"
        "  release:\n"
        "    env:\n"
        "      RELEASE_TAG: release-${{ github.sha }}\n"
        "    steps:\n"
        "      - run: gh release create \"$RELEASE_TAG\" dist/demo.zip --prerelease --target \"$GITHUB_SHA\"\n",
        encoding="utf-8",
    )
    findings = check_profiles(root, repository("tool", "rust"))
    manual = [finding for finding in findings if not finding.fixable]
    assert any("profile dist" in finding.message for finding in manual)


def test_bare_release_in_makefile_target_is_manual(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    (root / "src").mkdir()
    (root / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
    write_manifest(root, thin_release())
    (root / "Makefile").write_text(
        "release:\n\tcargo build --release -p demo\n", encoding="utf-8"
    )
    findings = check_profiles(root, repository("tool", "rust"))
    assert any(
        not finding.fixable and "Makefile" in (finding.file or "")
        for finding in findings
    )


def test_release_test_profile_is_not_flagged(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    (root / "src").mkdir()
    (root / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
    write_manifest(root, thin_release())
    workflows = root / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "ci.yml").write_text(
        "name: ci\non: push\njobs:\n  test:\n    steps:\n"
        "      - run: cargo test --release\n",
        encoding="utf-8",
    )
    (root / "Makefile").write_text(
        "test-ci:\n\tcargo test --release\n", encoding="utf-8"
    )
    assert check_profiles(root, repository("tool", "rust")) == []
