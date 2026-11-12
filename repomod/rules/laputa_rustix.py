"""The cargo dependency `rustix` is preferred as `laputa-rustix`.

`laputa-rustix` is the laputa-systems fork of rustix with Windows support
removed. Fleet repositories resolve it from the stuff registry, which serves
it as the replacement for the crates.io release.

The fixer rewrites a direct `rustix` dependency to the same key with
`package = "laputa-rustix"`, so the import paths in dependent code stay
unchanged, and it refreshes `Cargo.lock` minimally.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

from repomod.rules.workspace import is_workspace_excluded, workspace_excluded
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_RULE_ID = "laputa-rustix"
_FORK = "laputa-rustix"
_PRUNED_DIRECTORIES = frozenset({".git", ".work", ".venv", "node_modules", "target"})
_SKIPPED_PARTS = frozenset({"fixtures", "testdata", "snapshots", "golden", "third_party", "vendor"})
_ALLOWED_PATHS = (
    "Cargo.toml",
    "*/Cargo.toml",
    "Cargo.lock",
    "*/Cargo.lock",
)
_DEPENDENCY_TABLE_NAMES = frozenset({"dependencies", "dev-dependencies", "build-dependencies"})
_KEY = ("rustix", "'rustix'", '"rustix"')


@dataclass(frozen=True, slots=True)
class _Occurrence:
    line: int
    column: int
    value_start: int
    value_end: int


def _walk(root: Path) -> list[Path]:
    excluded = workspace_excluded(root)
    found: list[Path] = []
    for directory, names, files in os.walk(root):
        names[:] = sorted(name for name in names if name not in _PRUNED_DIRECTORIES)
        base = Path(directory)
        relative = PurePosixPath(base.relative_to(root).as_posix())
        if relative.parts and is_workspace_excluded(excluded, relative):
            names[:] = []
            continue
        for name in files:
            path = base / name
            parts = PurePosixPath(path.relative_to(root).as_posix()).parts
            if any(part.casefold() in _SKIPPED_PARTS for part in parts):
                continue
            found.append(path)
    return sorted(found)


def _split_toml_path(text: str) -> list[str] | None:
    parts: list[str] = []
    current: list[str] = []
    quote: str | None = None
    for char in text:
        if quote is not None:
            if char == quote:
                quote = None
            else:
                current.append(char)
            continue
        if char in "'\"":
            quote = char
            continue
        if char == ".":
            parts.append("".join(current).strip())
            current = []
            continue
        current.append(char)
    if quote is not None:
        return None
    parts.append("".join(current).strip())
    return parts


def _is_dependency_table(inner: str) -> bool:
    parts = _split_toml_path(inner)
    return bool(parts) and parts[-1] in _DEPENDENCY_TABLE_NAMES


def _table_header(line: str) -> str | None:
    stripped = line.strip()
    if not stripped.startswith("[") or stripped.startswith("[["):
        return None
    end = stripped.rfind("]")
    if end == -1:
        return None
    suffix = stripped[end + 1 :].strip()
    if suffix and not suffix.startswith("#"):
        return None
    return stripped[1:end]


def _is_key_line(line: str) -> bool:
    stripped = line.lstrip()
    for key in _KEY:
        if stripped.startswith(key):
            return stripped[len(key) :].lstrip().startswith("=")
    return False


def _value_span(source: str, start: int) -> tuple[int, int, str]:
    value_start = start
    while value_start < len(source) and source[value_start] in " \t":
        value_start += 1
    index = value_start
    depth = 0
    string: str | None = None
    while index < len(source):
        char = source[index]
        if string is not None:
            if char == "\\" and string == '"':
                index += 2
                continue
            if char == string:
                string = None
            index += 1
            continue
        if char in "\"'":
            string = char
            index += 1
            continue
        if char == "#":
            break
        if char in "{[":
            depth += 1
            index += 1
            continue
        if char in "}]":
            depth -= 1
            index += 1
            continue
        if char == "\n":
            if depth == 0:
                break
            index += 1
            continue
        index += 1
    return value_start, index, source[value_start:index].strip()


def _dependency_occurrences(source: str) -> list[_Occurrence]:
    occurrences: list[_Occurrence] = []
    dependency = False
    offset = 0
    for lineno, line in enumerate(source.splitlines(keepends=True), start=1):
        header = _table_header(line)
        if header is not None:
            dependency = _is_dependency_table(header)
        elif dependency and _is_key_line(line):
            key_column = len(line) - len(line.lstrip())
            equals = line.index("=", key_column)
            value_start, value_end, _ = _value_span(source, offset + equals + 1)
            occurrences.append(_Occurrence(lineno, key_column + 1, value_start, value_end))
        offset += len(line)
    return occurrences


def _spec_of(source: str, occurrence: _Occurrence) -> object | None:
    text = source[occurrence.value_start:occurrence.value_end]
    try:
        return tomllib.loads(f"rustix = {text}\n")["rustix"]
    except tomllib.TOMLDecodeError:
        return None


def _classify(spec: object) -> str:
    if isinstance(spec, dict):
        package = spec.get("package")
        if package == _FORK:
            return "migrated"
        if isinstance(package, str):
            return "other"
        if package is not None:
            return "unknown"
        if "path" in spec or "git" in spec:
            return "local"
        return "swap"
    if isinstance(spec, str):
        return "swap"
    return "unknown"


def _read(root: Path, relative: PurePosixPath) -> tuple[str | None, str | None]:
    path = root.joinpath(*relative.parts)
    if path.is_symlink():
        return None, "manifest is a symlink"
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        return None, f"cannot read manifest: {error}"
    try:
        tomllib.loads(source)
    except tomllib.TOMLDecodeError as error:
        return None, f"manifest is invalid TOML: {error}"
    return source, None


def laputa_rustix_findings(root: Path, repository: Repository) -> list[Finding]:
    findings: list[Finding] = []
    for path in _walk(root):
        if path.name != "Cargo.toml":
            continue
        relative = PurePosixPath(path.relative_to(root).as_posix())
        source, problem = _read(root, relative)
        if source is None:
            findings.append(
                operational(repository, _RULE_ID, relative.as_posix(), problem or "unreadable manifest")
            )
            continue
        for occurrence in _dependency_occurrences(source):
            spec = _spec_of(source, occurrence)
            if spec is None:
                findings.append(
                    operational(
                        repository,
                        _RULE_ID,
                        relative.as_posix(),
                        "cannot parse the `rustix` dependency specification",
                    )
                )
                continue
            decision = _classify(spec)
            if decision == "unknown":
                findings.append(
                    operational(
                        repository,
                        _RULE_ID,
                        relative.as_posix(),
                        "dependency `rustix` has an unrecognizable specification",
                    )
                )
                continue
            if decision != "swap":
                continue
            findings.append(
                violation(
                    repository,
                    _RULE_ID,
                    relative.as_posix(),
                    "dependency `rustix` is preferred as `laputa-rustix` "
                    "(the laputa-systems fork of rustix without Windows support)",
                    fixable=True,
                    line=occurrence.line,
                    column=occurrence.column,
                )
            )
    return sorted(
        findings,
        key=lambda finding: (finding.file or "", finding.line or 0, finding.column or 0),
    )


def _rewrite(source: str) -> str:
    result = source
    occurrences = _dependency_occurrences(result)
    for occurrence in sorted(occurrences, key=lambda item: item.value_start, reverse=True):
        if _classify(_spec_of(result, occurrence)) != "swap":
            continue
        raw = result[occurrence.value_start : occurrence.value_end]
        text = raw.strip()
        if text.startswith("{"):
            inner = raw[raw.index("{") + 1 :].lstrip(" \t")
            replacement = '{ package = "laputa-rustix", ' + inner
        else:
            replacement = f'{{ package = "laputa-rustix", version = {text} }}'
        result = result[: occurrence.value_start] + replacement + result[occurrence.value_end :]
    return result


def _lock_directories(work: Path, manifests: list[str]) -> list[str]:
    directories: set[str] = set()
    for manifest in manifests:
        parts = PurePosixPath(manifest).parent.parts
        for depth in range(len(parts), -1, -1):
            directory = work.joinpath(*parts[:depth]) if depth else work
            if (directory / "Cargo.lock").is_file():
                directories.add(PurePosixPath(*parts[:depth]).as_posix() if depth else ".")
                break
    return sorted(directories)


def _refresh_locks(root: Path, manifests: list[str], plan: dict[str, str]) -> None:
    if shutil.which("cargo") is None:
        raise RuntimeError("cargo is required to refresh Cargo.lock and was not found")
    with tempfile.TemporaryDirectory(prefix="repomod-laputa-rustix-") as temporary:
        work = Path(temporary) / "work"
        shutil.copytree(
            root,
            work,
            symlinks=True,
            ignore=lambda _dir, names: {name for name in names if name in _PRUNED_DIRECTORIES},
        )
        for relative, text in plan.items():
            target = work / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text, encoding="utf-8")
        for directory in _lock_directories(work, manifests):
            project = work if directory == "." else work / directory
            environment = {**os.environ, "CARGO_TERM_COLOR": "never"}
            result = subprocess.run(
                ["cargo", "metadata", "--format-version", "1", "--color", "never"],
                cwd=project,
                env=environment,
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=600,
                check=False,
            )
            if result.returncode != 0:
                detail = (result.stderr or result.stdout).strip()
                raise RuntimeError(f"cargo metadata failed in {directory}: {detail}")
            lock = PurePosixPath(directory, "Cargo.lock") if directory != "." else PurePosixPath("Cargo.lock")
            plan[lock.as_posix()] = (project / "Cargo.lock").read_text(encoding="utf-8")


def laputa_rustix_plan(root: Path, repository: Repository) -> dict[str, str]:
    plan: dict[str, str] = {}
    for finding in laputa_rustix_findings(root, repository):
        if not finding.fixable or finding.file is None or finding.file in plan:
            continue
        plan[finding.file] = _rewrite((root / finding.file).read_text(encoding="utf-8"))
    manifests = [name for name in plan if name.endswith("Cargo.toml")]
    if manifests:
        _refresh_locks(root, manifests, plan)
    return plan


def _check(root: Path, repository: Repository) -> list[Finding]:
    return laputa_rustix_findings(root, repository)


def _fix(root: Path, repository: Repository) -> dict[str, str]:
    return laputa_rustix_plan(root, repository)


def laputa_rustix_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            _RULE_ID,
            "Prefer laputa-rustix over the crates.io rustix crate",
            TagPredicate(),
            _check,
            _fix,
            "chore(repomod): prefer laputa-rustix over rustix",
            _ALLOWED_PATHS,
        ),
    )
