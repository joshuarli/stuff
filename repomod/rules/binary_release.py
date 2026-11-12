from __future__ import annotations

import glob
import re
import tomllib
from pathlib import Path

from repomod.catalog import LANGUAGE_TAGS, PLATFORM_TAGS
from repomod.rules.release import _POLICY as _RELEASE_POLICY
from repomod.rules.release_notes import (
    RELEASE_NOTES_FLAG,
    RELEASE_NOTES_STEP_NAME,
)
from repomod.rules.release_notes import render_workflow_step as render_release_notes_step
from repomod.rules.static_elf import STATIC_SMOKE_NAME, render_workflow_step
from repomod.rules.workflows import read_workflow_sources
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_BINARY_POLICY = _RELEASE_POLICY["binary"]
_PORTABLE_TARGETS = frozenset(_BINARY_POLICY["portable_targets"])
_MACOS_TARGETS = frozenset(_BINARY_POLICY["macos_targets"])
_LINUX_ARCHES = frozenset(_BINARY_POLICY["linux_arches"])
_LINUX_MUSL_SUFFIX = _BINARY_POLICY["linux_musl_suffix"]
_FORBIDDEN_TARGET_MARKERS = tuple(_BINARY_POLICY["forbidden_target_markers"])
_NON_NATIVE_RELEASE_TAGS = frozenset({"extension", "build"})
_BINARY_TOOL_EXTRA_TAGS = frozenset({"fork"}) | PLATFORM_TAGS | LANGUAGE_TAGS

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
_API_TAG_CREATE = re.compile(
    r"gh\s+api\s+[\"']?repos/\$REPOSITORY/git/refs[\"']?\s+-f\s+[\"']?ref=refs/tags/\$RELEASE_TAG[\"']?\s+-f\s+[\"']?sha=\$GITHUB_SHA",
    re.IGNORECASE,
)
_VERIFY_TAG = re.compile(r"--verify-tag\b", re.IGNORECASE)
_DEFAULT_BRANCH_GUARD = re.compile(r"default_branch", re.IGNORECASE)
_DEFAULT_BRANCH_REF = re.compile(r"refs/heads/", re.IGNORECASE)
_CHECKSUMS_FILE = re.compile(r"SHA256SUMS")
_TRIPLE = re.compile(
    r"\b(aarch64-apple-darwin|x86_64-apple-darwin|(?:x86_64|aarch64)(?:-unknown)?-linux-(?:musl|gnu(?:eabihf)?))\b"
)
_NATIVE_ASSET = re.compile(r"\.(?:zip|tar\.gz|tgz|dmg|exe|msi|deb|rpm)\b", re.IGNORECASE)
_PLATFORM_TARGET = re.compile(
    r"(?:aarch64|arm64|x86_64|x64|amd64|apple-darwin|linux|windows|macos)",
    re.IGNORECASE,
)
_MAKE_RELEASE = re.compile(r"\bmake\s+(?:[^\n]*\s)?(?:release(?:-[a-z0-9_-]+)?|dist)\b", re.IGNORECASE)
_ARTIFACT_UPLOAD = re.compile(r"(?im)^\s*(?:-\s*)?uses\s*:\s*actions/upload-artifact@")
_ARTIFACT_DOWNLOAD = re.compile(r"(?im)^\s*(?:-\s*)?uses\s*:\s*actions/download-artifact@")
_STATIC_SMOKE_STEP = re.compile(
    rf"(?m)^(?P<indent> *)-\s+name:\s*{re.escape(STATIC_SMOKE_NAME)}\s*$"
)
_DOWNLOAD_STEP = re.compile(r"(?m)^(?P<indent> *)-\s+uses:\s*actions/download-artifact@")
_NOTES_STEP = re.compile(
    rf"(?m)^(?P<indent> *)-\s+name:\s*{re.escape(RELEASE_NOTES_STEP_NAME)}\s*$"
)
_PRERELEASE_FLAG = re.compile(r"(?<!\S)--prerelease\b")
_JOB_KEY = re.compile(r"^  ([A-Za-z0-9_-]+):\s*$")


def _read_workflows(
    root: Path, repository: Repository | None = None
) -> tuple[list[tuple[str, str]], list[tuple[str, str]]]:
    sources, problems = read_workflow_sources(root, repository)
    workflows = [
        (
            relative,
            "\n".join(line for line in source.splitlines() if not line.lstrip().startswith("#")),
        )
        for relative, source in sources
    ]
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
        if re.match(r"^(?:release(?:-[A-Za-z0-9_-]+)?|dist)\s*:", line) is None:
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


def _c_has_executable(root: Path) -> bool:
    makefile = root / "Makefile"
    if makefile.is_symlink() or not makefile.is_file():
        return False
    try:
        text = makefile.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return False
    if re.search(r"(?m)^\s*(?:CC|CFLAGS|LDFLAGS)\s*[:?+!]?=", text) is None:
        return False
    for pattern in ("*.c", "src/*.c", "src/*/*.c"):
        try:
            candidates = glob.glob(str(root / pattern))
        except OSError:
            continue
        for name in candidates:
            path = Path(name)
            if not path.is_file() or path.is_symlink():
                continue
            try:
                if path.resolve().is_relative_to(root.resolve()):
                    return True
            except OSError:
                continue
    return False


def _make_c_build(root: Path, source: str) -> bool:
    if re.search(r"\bmake\b", source) is None:
        return False
    return _c_has_executable(root)


def _docker_c_build(root: Path, source: str) -> bool:
    if re.search(r"\bmake\b", source) is None:
        return False
    dockerfile = root / "Dockerfile"
    if dockerfile.is_symlink() or not dockerfile.is_file():
        return False
    try:
        text = dockerfile.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return False
    if re.search(r"git-.*\.tar\.xz", text) is None:
        return False
    if re.search(r"\bmake\b", text) is None:
        return False
    return re.search(r"\b(?:CC|CFLAGS)\b", text) is not None


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
    c_make = _make_c_build(root, source)
    c_docker = _docker_c_build(root, source)
    return any((rust, go, swift, cmake, zig, zig_make, c_make, c_docker))


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
        and (_SOURCE_TARGET.search(source) is not None or _API_TAG_CREATE.search(source) is not None)
    )
    if binary_contract is True:
        return "binary"
    return "ambiguous"


def has_binary_release_contract(root: Path, repository: Repository | None = None) -> bool:
    workflows, _ = _read_workflows(root, repository)
    return any(_release_kind(root, source) == "binary" for _, source in workflows)


def _binary_release_findings(root: Path, repository: Repository) -> list[Finding]:
    if repository.private:
        return []
    workflows, problems = _read_workflows(root, repository)
    findings = [
        operational(repository, "binary-release-tags", path, message) for path, message in problems
    ]
    for path, source in workflows:
        kind = _release_kind(root, source)
        if kind == "none" or kind == "package":
            continue
        if kind == "ambiguous":
            if _NON_NATIVE_RELEASE_TAGS & set(repository.tags):
                continue
            findings.append(
                violation(
                    repository,
                    "binary-release-tags",
                    path,
                    "GitHub prerelease workflow does not establish the supported native binary contract; "
                    "review its artifact type and release target",
                    severity="warning",
                )
            )
            continue
        tags = set(repository.tags)
        if "tool" not in tags:
            findings.append(
                violation(
                    repository,
                    "binary-release-tags",
                    path,
                    "standard binary prerelease repositories require the tool role",
                )
            )
            continue
        extras = sorted(tags - {"tool"} - _BINARY_TOOL_EXTRA_TAGS)
        if extras:
            findings.append(
                violation(
                    repository,
                    "binary-release-tags",
                    path,
                    "standard binary prerelease repositories allow only tool with fork, "
                    "platform, and language tags: remove " + ", ".join(extras),
                )
            )
    return findings


def _has_prerelease_intent(source: str) -> bool:
    return _PRERELEASE_COMMAND.search(source) is not None or (
        _RELEASE_ACTION.search(source) is not None and _PRERELEASE_INPUT.search(source) is not None
    )


def _prerelease_workflows(
    root: Path, repository: Repository
) -> tuple[list[tuple[str, str]], list[Finding]]:
    workflows, problems = _read_workflows(root, repository)
    findings = [
        operational(repository, "binary-release-standard", path, message)
        for path, message in problems
    ]
    return [(path, source) for path, source in workflows if _has_prerelease_intent(source)], findings


def _standard_scope(tags: set[str]) -> str:
    if "linux" in tags and "macos" not in tags:
        return "linux"
    if "macos" in tags and "linux" not in tags:
        return "macos"
    return "portable"


def _standard_target_findings(
    repository: Repository, path: str, source: str, scope: str
) -> list[Finding]:
    lowered = source.casefold()
    markers = sorted({marker for marker in _FORBIDDEN_TARGET_MARKERS if marker in lowered})
    if markers:
        return [
            violation(
                repository,
                "binary-release-standard",
                path,
                "binary prerelease targets must be musl or Apple silicon; "
                "remove glibc markers: " + ", ".join(markers),
            )
        ]
    found = {match.group(1).lower() for match in _TRIPLE.finditer(source)}
    if scope == "macos":
        if found == set(_MACOS_TARGETS):
            return []
        return [
            violation(
                repository,
                "binary-release-standard",
                path,
                "macOS-only binary prerelease must build exactly "
                + ", ".join(sorted(_MACOS_TARGETS))
                + "; found: "
                + (", ".join(sorted(found)) or "no release targets"),
            )
        ]
    if scope == "linux":
        arches = {target[: target.index("-")] for target in found}
        if (
            found
            and arches == set(_LINUX_ARCHES)
            and all(target.endswith(_LINUX_MUSL_SUFFIX) for target in found)
            and not any("darwin" in target for target in found)
        ):
            return []
        return [
            violation(
                repository,
                "binary-release-standard",
                path,
                "Linux-only binary prerelease must build the musl targets for "
                + ", ".join(sorted(_LINUX_ARCHES))
                + " with no darwin or glibc targets; found: "
                + (", ".join(sorted(found)) or "no release targets"),
            )
        ]
    if found == set(_PORTABLE_TARGETS):
        return []
    return [
        violation(
            repository,
            "binary-release-standard",
            path,
            "portable binary prerelease must build exactly "
            + ", ".join(sorted(_PORTABLE_TARGETS))
            + " with no glibc targets; found: "
            + (", ".join(sorted(found)) or "no release targets"),
        )
    ]


def _standard_format_findings(
    repository: Repository, path: str, source: str
) -> list[Finding]:
    findings = []
    if _WORKFLOW_DISPATCH.search(source) is None:
        findings.append(
            violation(
                repository,
                "binary-release-standard",
                path,
                "binary prerelease must run on workflow_dispatch like ish",
            )
        )
    if _SHA_RELEASE_TAG.search(source) is None:
        findings.append(
            violation(
                repository,
                "binary-release-standard",
                path,
                "binary prerelease must publish under a release-${{ github.sha }} tag like ish",
            )
        )
    if _SOURCE_TARGET.search(source) is None and _API_TAG_CREATE.search(source) is None:
        findings.append(
            violation(
                repository,
                "binary-release-standard",
                path,
                "binary prerelease must target the triggering commit like ish",
            )
        )
    if _NATIVE_ASSET.search(source) is None or _CHECKSUMS_FILE.search(source) is None:
        findings.append(
            violation(
                repository,
                "binary-release-standard",
                path,
                "binary prerelease must publish per-platform zip archives with a "
                "verified SHA256SUMS file like ish",
            )
        )
    if _API_TAG_CREATE.search(source) is None or _VERIFY_TAG.search(source) is None:
        findings.append(
            violation(
                repository,
                "binary-release-standard",
                path,
                "binary prerelease must create the release tag at the triggering commit "
                "through the GitHub API and pass --verify-tag to gh release create like ish",
            )
        )
    if _DEFAULT_BRANCH_GUARD.search(source) is None or _DEFAULT_BRANCH_REF.search(source) is None:
        findings.append(
            violation(
                repository,
                "binary-release-standard",
                path,
                "binary prerelease must refuse dispatches off the default branch "
                "with an ish-style preflight ref check",
            )
        )
    return findings


def _binary_release_standard_findings(root: Path, repository: Repository) -> list[Finding]:
    if repository.private or "fork" in repository.tags:
        return []
    workflows, findings = _prerelease_workflows(root, repository)
    scope = _standard_scope(set(repository.tags))
    for path, source in workflows:
        findings.extend(_standard_target_findings(repository, path, source, scope))
        findings.extend(_standard_format_findings(repository, path, source))
    return findings


def _workflow_without_comments(source: str) -> str:
    return "\n".join(
        "" if line.lstrip().startswith("#") else line for line in source.splitlines()
    )


def _job_ranges(source: str) -> list[tuple[str, int, int]]:
    lines = source.splitlines()
    jobs_start = next(
        (index for index, line in enumerate(lines) if re.fullmatch(r"jobs:\s*", line)),
        None,
    )
    if jobs_start is None:
        return []
    jobs_end = len(lines)
    for index in range(jobs_start + 1, len(lines)):
        line = lines[index]
        if line and not line.startswith((" ", "\t", "#")):
            jobs_end = index
            break
    starts = []
    for index in range(jobs_start + 1, jobs_end):
        match = _JOB_KEY.fullmatch(lines[index])
        if match is not None:
            starts.append((match.group(1), index))
    return [
        (name, start, starts[index + 1][1] if index + 1 < len(starts) else jobs_end)
        for index, (name, start) in enumerate(starts)
    ]


def _step_end(lines: list[str], start: int, end: int, indentation: int) -> int:
    for index in range(start + 1, end):
        line = lines[index]
        if (
            line.strip()
            and len(line) - len(line.lstrip(" ")) == indentation
            and line[indentation:].startswith("-")
        ):
            return index
    return end


def _static_smoke_valid(source: str, image_mode: bool) -> bool:
    lines = source.splitlines()
    smoke = _STATIC_SMOKE_STEP.search(source)
    download = _DOWNLOAD_STEP.search(source)
    publishers = [
        match.start()
        for pattern in (_PRERELEASE_COMMAND, _RELEASE_ACTION)
        if (match := pattern.search(source)) is not None
    ]
    if smoke is None or download is None or not publishers:
        return False
    if not download.start() < smoke.start() < min(publishers):
        return False
    start = source.count("\n", 0, smoke.start())
    indentation = len(smoke.group("indent"))
    end = _step_end(lines, start, len(lines), indentation)
    block = "\n".join(lines[start:end])
    return block.rstrip() == "\n".join(render_workflow_step(indentation, image_mode)).rstrip()


def _insert_static_smoke(source: str, image_mode: bool) -> str | None:
    lines = source.splitlines()
    clean_lines = _workflow_without_comments(source).splitlines()
    ranges = _publish_job_ranges(source)
    if not ranges:
        return None
    insertions: list[tuple[int, int, list[str]]] = []
    for job_start, job_end in ranges:
        job_source = "\n".join(clean_lines[job_start:job_end])
        if _static_smoke_valid(job_source, image_mode):
            continue
        download = _DOWNLOAD_STEP.search(job_source)
        if download is None:
            return None
        download_line = job_start + job_source.count("\n", 0, download.start())
        indentation = len(download.group("indent"))
        step_end = _step_end(lines, download_line, job_end, indentation)
        for existing in _STATIC_SMOKE_STEP.finditer(job_source):
            start = job_start + job_source.count("\n", 0, existing.start())
            end = _step_end(lines, start, job_end, indentation)
            insertions.append((start, end, []))
        insertions.append((step_end, step_end, render_workflow_step(indentation, image_mode)))
    for start, end, block in sorted(insertions, reverse=True):
        before = lines[:start]
        after = lines[end:]
        while before and not before[-1].strip():
            before.pop()
        while after and not after[0].strip():
            after.pop(0)
        lines = [*before, "", *block, "", *after]
    return "\n".join(lines) + "\n"


def _publish_job_ranges(source: str) -> list[tuple[int, int]]:
    lines = _workflow_without_comments(source).splitlines()
    return [
        (start, end)
        for _, start, end in _job_ranges("\n".join(lines))
        if _has_prerelease_intent("\n".join(lines[start:end]))
    ]


def _image_mode(root: Path) -> bool:
    from repomod.rules.musl_toolchain import (
        _FROM,
        _arg_values,
        _expand,
        _logical_lines,
        _split_image,
        load_musl_toolchain,
    )

    try:
        lock = load_musl_toolchain()
    except (OSError, ValueError):
        return False
    if lock.image is None:
        return False
    dockerfile = root / "Dockerfile"
    if dockerfile.is_symlink() or not dockerfile.is_file():
        return False
    try:
        source = dockerfile.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return False
    arguments = _arg_values(_logical_lines(source))
    for line in _logical_lines(source):
        match = _FROM.fullmatch(line.text)
        if match is None:
            continue
        image = _expand(match.group("image"), arguments)
        repo, _, digest = _split_image(image)
        if repo.casefold() == lock.image.reference.casefold() and digest == lock.image.digest:
            return True
    return False


def _binary_release_static_findings(root: Path, repository: Repository) -> list[Finding]:
    if repository.private or "macos" in repository.tags:
        return []
    image_mode = _image_mode(root)
    sources, problems = read_workflow_sources(root)
    findings = [
        operational(repository, "binary-release-static", path, message)
        for path, message in problems
    ]
    for path, source in sources:
        clean = _workflow_without_comments(source)
        if _release_kind(root, clean) != "binary":
            continue
        jobs = _publish_job_ranges(clean)
        if jobs and all(
            _static_smoke_valid("\n".join(clean.splitlines()[start:end]), image_mode)
            for start, end in jobs
        ):
            continue
        fixable = bool(jobs) and all(
            _DOWNLOAD_STEP.search("\n".join(clean.splitlines()[start:end])) is not None
            for start, end in jobs
        )
        findings.append(
            violation(
                repository,
                "binary-release-static",
                path,
                "binary prerelease must run the shared static ELF verifier on downloaded Linux "
                "artifacts before publication",
                fixable=fixable,
            )
        )
    return findings


def _binary_release_static_fix(root: Path, repository: Repository) -> dict[str, str]:
    image_mode = _image_mode(root)
    sources, _ = read_workflow_sources(root)
    plan = {}
    for path, source in sources:
        clean = _workflow_without_comments(source)
        if _release_kind(root, clean) != "binary":
            continue
        changed = _insert_static_smoke(source, image_mode)
        if changed is not None and changed != source:
            plan[path] = changed
    return plan


def _containing_step(lines: list[str], line: int, job_start: int) -> tuple[int, int] | None:
    for index in range(line, job_start, -1):
        stripped = lines[index].lstrip(" ")
        if stripped.startswith("- ") and len(lines[index]) - len(stripped) <= len(lines[line]) - len(
            lines[line].lstrip(" ")
        ):
            return index, len(lines[index]) - len(stripped)
    return None


def _trimmed(lines: list[str]) -> list[str]:
    end = len(lines)
    while end and not lines[end - 1].strip():
        end -= 1
    return lines[:end]


def _release_notes_job_edits(
    lines: list[str], clean_lines: list[str], job_start: int, job_end: int
) -> list[tuple[int, int, list[str]]] | None:
    job_source = "\n".join(clean_lines[job_start:job_end])
    create = _PRERELEASE_COMMAND.search(job_source)
    if create is None:
        return None
    create_line = job_start + job_source.count("\n", 0, create.start())
    located = _containing_step(clean_lines, create_line, job_start)
    if located is None:
        return None
    create_start, indentation = located
    create_end = _step_end(lines, create_start, job_end, indentation)
    rendered = render_release_notes_step(indentation)
    notes_starts = [
        job_start + job_source.count("\n", 0, match.start())
        for match in _NOTES_STEP.finditer(job_source)
    ]
    notes_steps = [
        (start, _step_end(lines, start, job_end, indentation)) for start in notes_starts
    ]
    create_block = lines[create_start:create_end]
    has_flag = RELEASE_NOTES_FLAG in "\n".join(clean_lines[create_start:create_end])
    if (
        has_flag
        and len(notes_steps) == 1
        and notes_steps[0][1] <= create_start
        and _trimmed(lines[notes_steps[0][0] : notes_steps[0][1]]) == rendered
    ):
        return []
    if not has_flag:
        offset = create_line - create_start
        for index in range(offset, len(create_block)):
            if _PRERELEASE_FLAG.search(clean_lines[create_start + index]) is not None:
                create_block[index] = _PRERELEASE_FLAG.sub(
                    f"--prerelease {RELEASE_NOTES_FLAG}", create_block[index], count=1
                )
                break
        else:
            return None
    edits: list[tuple[int, int, list[str]]] = [(start, end, []) for start, end in notes_steps]
    edits.append((create_start, create_end, [*rendered, "", *_trimmed(create_block)]))
    return edits


def _release_notes_edits(source: str) -> list[tuple[int, int, list[str]]] | None:
    lines = source.splitlines()
    clean_lines = _workflow_without_comments(source).splitlines()
    ranges = _publish_job_ranges(source)
    if not ranges:
        return None
    edits = []
    for job_start, job_end in ranges:
        job_edits = _release_notes_job_edits(lines, clean_lines, job_start, job_end)
        if job_edits is None:
            return None
        edits.extend(job_edits)
    return edits


def _apply_edits(source: str, edits: list[tuple[int, int, list[str]]]) -> str:
    lines = source.splitlines()
    for start, end, block in sorted(edits, reverse=True):
        before = lines[:start]
        after = lines[end:]
        while before and not before[-1].strip():
            before.pop()
        while after and not after[0].strip():
            after.pop(0)
        middle = [*before, "", *block] if block else before
        lines = [*middle, "", *after] if after else middle
    return "\n".join(lines) + "\n"


def _binary_release_notes_findings(root: Path, repository: Repository) -> list[Finding]:
    if repository.private:
        return []
    sources, problems = read_workflow_sources(root)
    findings = [
        operational(repository, "binary-release-notes", path, message)
        for path, message in problems
    ]
    for path, source in sources:
        clean = _workflow_without_comments(source)
        if _release_kind(root, clean) != "binary":
            continue
        edits = _release_notes_edits(source)
        if edits == []:
            continue
        findings.append(
            violation(
                repository,
                "binary-release-notes",
                path,
                "binary prerelease must publish the commit changelog since the previous release: "
                f"run the shared '{RELEASE_NOTES_STEP_NAME}' step before gh release create and pass "
                f"{RELEASE_NOTES_FLAG}",
                fixable=edits is not None,
            )
        )
    return findings


def _binary_release_notes_fix(root: Path, repository: Repository) -> dict[str, str]:
    sources, _ = read_workflow_sources(root)
    plan = {}
    for path, source in sources:
        if _release_kind(root, _workflow_without_comments(source)) != "binary":
            continue
        edits = _release_notes_edits(source)
        if edits is not None and edits:
            plan[path] = _apply_edits(source, edits)
    return plan


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
        Rule(
            "binary-release-standard",
            "Check the standardized native binary prerelease matrix and format",
            TagPredicate(any_of=frozenset({"tool"})),
            _binary_release_standard_findings,
            None,
            None,
            (),
        ),
        Rule(
            "binary-release-static",
            "Require static Linux ELF smoke checks before native binary prereleases",
            TagPredicate(any_of=frozenset({"tool"})),
            _binary_release_static_findings,
            _binary_release_static_fix,
            "fix(release): verify static Linux artifacts",
            (".github/workflows/*.yml", ".github/workflows/*.yaml"),
        ),
        Rule(
            "binary-release-notes",
            "Require commit changelogs in native binary prerelease notes",
            TagPredicate(any_of=frozenset({"tool"})),
            _binary_release_notes_findings,
            _binary_release_notes_fix,
            "fix(release): publish commit changelogs in prerelease notes",
            (".github/workflows/*.yml", ".github/workflows/*.yaml"),
        ),
    )
