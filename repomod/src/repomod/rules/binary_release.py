from __future__ import annotations

import glob
import re
import tomllib
from pathlib import Path

from repomod.rules.workflows import workflow_files
from repomod.types import Finding, Repository, Rule, TagPredicate

_PRERELEASE_COMMAND = re.compile(
    r"\bgh\s+release\s+create\b[\s\S]*?(?:^|\s)--prerelease\b",
    re.IGNORECASE | re.MULTILINE,
)
_RELEASE_ACTION = re.compile(r"\bsoftprops/action-gh-release\b", re.IGNORECASE)
_PRERELEASE_INPUT = re.compile(r"(?m)^\s*prerelease\s*:\s*true\b", re.IGNORECASE)
_WORKFLOW_DISPATCH = re.compile(r"(?m)^\s*workflow_dispatch\s*:")
_SHA_RELEASE_TAG = re.compile(r"release-(?:\$\{\{\s*github\.sha\s*\}\}|\$\{?GITHUB_SHA\}?)", re.IGNORECASE)
_SOURCE_TARGET = re.compile(
    r"(?:--target\s+['\"]?\$\{?GITHUB_SHA\}?|target_commitish\s*:\s*\$\{\{\s*github\.sha\s*\}\})",
    re.IGNORECASE,
)
_NATIVE_ASSET = re.compile(r"\.(?:zip|tar\.gz|tgz|dmg|exe|msi|deb|rpm)\b", re.IGNORECASE)
_PLATFORM_TARGET = re.compile(
    r"(?:aarch64|arm64|x86_64|x64|amd64|apple-darwin|linux|windows|macos)",
    re.IGNORECASE,
)
_MAKE_RELEASE = re.compile(r"\bmake\s+(?:[^\n]*\s)?release(?:-[a-z0-9_-]+)?\b", re.IGNORECASE)
_ARTIFACT_UPLOAD = re.compile(r"(?im)^\s*(?:-\s*)?uses\s*:\s*actions/upload-artifact@")
_ARTIFACT_DOWNLOAD = re.compile(r"(?im)^\s*(?:-\s*)?uses\s*:\s*actions/download-artifact@")


def _read_workflows(root: Path) -> tuple[list[tuple[str, str]], list[tuple[str, str]]]:
    files, problems = workflow_files(root)
    workflows = []
    for path in files:
        try:
            source = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            problems.append((path.relative_to(root).as_posix(), f"cannot read workflow: {error}"))
            continue
        active = "\n".join(line for line in source.splitlines() if not line.lstrip().startswith("#"))
        workflows.append((path.relative_to(root).as_posix(), active))
    return workflows, problems


def _cargo_has_executable(root: Path) -> bool:
    root_manifest = root / "Cargo.toml"
    if root_manifest.is_symlink() or not root_manifest.is_file():
        return False
    try:
        root_document = tomllib.loads(root_manifest.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError):
        return False
    manifests = [(root_manifest, root_document)]
    workspace = root_document.get("workspace")
    members = workspace.get("members", []) if isinstance(workspace, dict) else []
    if isinstance(members, list):
        for member in members:
            if not isinstance(member, str):
                continue
            for name in glob.glob(str(root / member / "Cargo.toml")):
                path = Path(name)
                try:
                    if not path.is_file() or not path.resolve().is_relative_to(root.resolve()):
                        continue
                    document = tomllib.loads(path.read_text(encoding="utf-8"))
                except (OSError, UnicodeError, tomllib.TOMLDecodeError):
                    continue
                manifests.append((path, document))
    for manifest_path, document in manifests:
        package = document.get("package")
        if not isinstance(package, dict):
            continue
        bins = document.get("bin")
        if isinstance(bins, list) and bins:
            return True
        if package.get("autobins", True) is False:
            continue
        package_root = manifest_path.parent
        main = package_root / "src" / "main.rs"
        if main.is_file() and main.resolve().is_relative_to(root.resolve()):
            return True
        binaries = package_root / "src" / "bin"
        if binaries.is_dir() and not binaries.is_symlink() and any(
            candidate.is_file()
            and not candidate.is_symlink()
            and candidate.suffix == ".rs"
            for candidate in binaries.iterdir()
        ):
            return True
    return False


def _go_has_executable(root: Path, source: str) -> bool:
    go_manifest = root / "go.mod"
    if (
        re.search(r"\bgo\s+build\b", source) is None
        or not go_manifest.is_file()
        or go_manifest.is_symlink()
    ):
        return False
    files = list(root.glob("*.go"))
    command_directory = root / "cmd"
    if command_directory.is_dir() and not command_directory.is_symlink():
        for application in command_directory.iterdir():
            if application.is_dir() and not application.is_symlink():
                files.extend(application.glob("*.go"))
    for path in files:
        if path.name.endswith("_test.go") or path.is_symlink():
            continue
        try:
            if not path.resolve().is_relative_to(root.resolve()):
                continue
            if re.search(r"(?m)^\s*package\s+main\b", path.read_text(encoding="utf-8")):
                return True
        except (OSError, UnicodeError):
            continue
    return False


def _swift_has_executable(root: Path) -> bool:
    manifest = root / "Package.swift"
    if manifest.is_symlink() or not manifest.is_file():
        return False
    try:
        source = manifest.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return False
    return re.search(r"\bexecutableTarget\s*\(|\.executable\s*\(", source) is not None


def _make_release_build(root: Path, source: str) -> bool:
    if not _MAKE_RELEASE.search(source):
        return False
    makefile = root / "Makefile"
    if makefile.is_symlink() or not makefile.is_file():
        return False
    try:
        lines = makefile.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        return False
    for index, line in enumerate(lines):
        if re.match(r"^release(?:-[A-Za-z0-9_-]+)?\s*:", line) is None:
            continue
        recipe = []
        for following in lines[index + 1 :]:
            if following and not following[0].isspace():
                break
            recipe.append(following)
        joined = "\n".join(recipe)
        if re.search(r"\bcargo\b", joined) and re.search(r"\bbuild\b", joined):
            return _cargo_has_executable(root)
    return False


def _make_zig_build(root: Path, source: str) -> bool:
    if re.search(r"\bmake\b", source) is None:
        return False
    makefile = root / "Makefile"
    if makefile.is_symlink() or not makefile.is_file():
        return False
    try:
        lines = makefile.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        return False
    for index, line in enumerate(lines):
        if re.match(r"^[A-Za-z0-9_.-]+\s*:[^\n]*\.zig\b", line) is None:
            continue
        recipe = []
        for following in lines[index + 1 :]:
            if following and not following[0].isspace():
                break
            recipe.append(following)
        if any(re.search(r"\bzig\s+build-exe\b", command) for command in recipe):
            source_name = line.split(":", 1)[1].strip().split()[0]
            zig_source = root / source_name
            if not zig_source.is_symlink() and zig_source.is_file():
                try:
                    return zig_source.resolve().is_relative_to(root.resolve())
                except OSError:
                    return False
    return False


def _native_build(root: Path, source: str) -> bool:
    cargo_command = re.search(r"\bcargo\s+(?:build|dev\s+dist)\b", source)
    rust = (cargo_command is not None or _make_release_build(root, source)) and _cargo_has_executable(root)
    go = _go_has_executable(root, source)
    swift = re.search(r"\bswift\s+build\b", source) and _swift_has_executable(root)
    cmake_manifest = root / "CMakeLists.txt"
    cmake = (
        re.search(r"\bcmake\s+--build\b", source)
        and cmake_manifest.is_file()
        and not cmake_manifest.is_symlink()
        and re.search(r"\badd_executable\s*\(", cmake_manifest.read_text(encoding="utf-8"))
    )
    zig_manifest = root / "build.zig"
    zig = (
        re.search(r"\bzig\s+build\b", source)
        and zig_manifest.is_file()
        and not zig_manifest.is_symlink()
        and re.search(r"\baddExecutable\s*\(", zig_manifest.read_text(encoding="utf-8"))
    )
    zig_make = _make_zig_build(root, source)
    return any((rust, go, swift, cmake, zig, zig_make))


def _release_kind(root: Path, source: str) -> str:
    has_prerelease = _PRERELEASE_COMMAND.search(source) or (
        _RELEASE_ACTION.search(source) and _PRERELEASE_INPUT.search(source)
    )
    if not has_prerelease:
        return "none"
    wheel_artifacts = ".whl" in source.casefold()
    if wheel_artifacts:
        return "package"
    if re.search(r"\bcargo\s+build\b[^\n]*\s--lib\b", source) and (root / "Cargo.toml").is_file():
        return "package"
    native_build = _native_build(root, source)
    artifact_transfer = _ARTIFACT_UPLOAD.search(source) and _ARTIFACT_DOWNLOAD.search(source)
    native_assets = _NATIVE_ASSET.search(source) is not None or (
        artifact_transfer and _PLATFORM_TARGET.search(source) is not None
    )
    binary_contract = (
        _WORKFLOW_DISPATCH.search(source) is not None
        and native_build
        and native_assets
        and artifact_transfer
        and _SHA_RELEASE_TAG.search(source) is not None
        and _SOURCE_TARGET.search(source) is not None
    )
    if binary_contract is True:
        return "binary"
    return "ambiguous"


def _binary_release_findings(root: Path, repository: Repository) -> list[Finding]:
    workflows, problems = _read_workflows(root)
    findings = [
        Finding(str(repository.id), "binary-release-tags", path, message, kind="operational")
        for path, message in problems
    ]
    for path, source in workflows:
        kind = _release_kind(root, source)
        if kind == "none" or kind == "package":
            continue
        if kind == "ambiguous":
            findings.append(
                Finding(
                    str(repository.id),
                    "binary-release-tags",
                    path,
                    "GitHub prerelease workflow does not establish the supported native binary contract; "
                    "review its artifact type and release target",
                    severity="warning",
                )
            )
            continue
        tags = set(repository.tags)
        if tags != {"tool"}:
            extras = ", ".join(sorted(tags - {"tool"}))
            details = []
            if "tool" not in tags:
                details.append("add tool")
            if extras:
                details.append(f"remove {extras}")
            findings.append(
                Finding(
                    str(repository.id),
                    "binary-release-tags",
                    path,
                    "standard binary prerelease repositories require exactly [tool]: " + "; ".join(details),
                )
            )
    return findings


def binary_release_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "binary-release-tags",
            "Check curated tags for the supported native binary prerelease workflow",
            TagPredicate(),
            _binary_release_findings,
            None,
            None,
            (),
        ),
    )
