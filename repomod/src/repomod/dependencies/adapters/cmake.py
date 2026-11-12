from __future__ import annotations

import posixpath
from collections.abc import Sequence
from dataclasses import dataclass
from typing import ClassVar

from repomod.dependencies.model import (
    AdapterResult,
    Component,
    CoverageKind,
    Ecosystem,
    Edge,
    Evidence,
    InputContent,
    Instance,
    Issue,
    Project,
    Relation,
    Resolution,
    Scope,
    SourceKind,
    purl,
    redact_url,
)

_PACKAGE_KEYWORDS = frozenset(
    {
        "REQUIRED",
        "QUIET",
        "COMPONENTS",
        "OPTIONAL_COMPONENTS",
        "CONFIG",
        "MODULE",
        "NO_MODULE",
        "EXACT",
        "GLOBAL",
        "IMPORTED_TARGET",
        "NO_CMAKE_PATH",
        "NO_CMAKE_ENVIRONMENT_PATH",
        "NO_SYSTEM_ENVIRONMENT_PATH",
        "NO_CMAKE_PACKAGE_REGISTRY",
        "NO_CMAKE_BUILDS_PATH",
        "NO_CMAKE_SYSTEM_PATH",
        "NO_CMAKE_INSTALL_PREFIX",
    }
)
_DECLARE_KEYWORDS = frozenset(
    {
        "URL",
        "URL_HASH",
        "URL_MD5",
        "GIT_REPOSITORY",
        "GIT_TAG",
        "GIT_SHALLOW",
        "SOURCE_DIR",
        "BINARY_DIR",
        "DOWNLOAD_EXTRACT_TIMESTAMP",
        "DOWNLOAD_NO_EXTRACT",
        "TIMEOUT",
        "INACTIVITY_TIMEOUT",
        "EXCLUDE_FROM_ALL",
        "USES_TERMINAL_DOWNLOAD",
        "USES_TERMINAL_UPDATE",
        "USES_TERMINAL_CONFIGURE",
        "USES_TERMINAL_BUILD",
        "USES_TERMINAL_INSTALL",
        "USES_TERMINAL_TEST",
        "TLS_VERIFY",
        "TLS_CAINFO",
        "NETRC",
        "HTTP_USERNAME",
        "HTTP_PASSWORD",
    }
)


@dataclass(frozen=True, slots=True)
class CmakeAdapter:
    name: ClassVar[str] = "cmake"

    def matches(self, path: str) -> bool:
        return path.rsplit("/", 1)[-1] == "CMakeLists.txt"

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        projects: dict[str, Project] = {}
        drafts: dict[str, _Ref] = {}
        edges: list[Edge] = []
        for content in inputs:
            directory = posixpath.dirname(content.path) or "."
            project = Project(f"{directory}:generic", Ecosystem.GENERIC, directory)
            projects.setdefault(project.id, project)
            for command in _commands(content.text):
                for ref, scope, resolution, constraint in _resolve_command(
                    command, content.path, content.vendored, issues
                ):
                    drafts.setdefault(ref.key, ref)
                    edges.append(
                        Edge(
                            from_id=project.id,
                            to_id=ref.instance_id,
                            relation=Relation.DIRECT,
                            scope=scope,
                            optional=False,
                            constraint=constraint,
                            resolved_version=None,
                            condition=None,
                            source_kind=ref.source_kind,
                            resolution=resolution,
                            evidence=(Evidence(content.path, f"L{command.line}"),),
                        )
                    )
        components: dict[str, Component] = {}
        instances: list[Instance] = []
        for ref in drafts.values():
            component = Component(ref.component, Ecosystem.GENERIC, ref.name, ref.name)
            components.setdefault(component.purl, component)
            instances.append(
                Instance(
                    id=ref.instance_id,
                    purl=ref.purl,
                    component=ref.component,
                    version=None,
                    source_kind=ref.source_kind,
                    vcs_url=ref.vcs_url,
                    commit=ref.commit,
                )
            )
        return AdapterResult(
            projects=tuple(projects.values()),
            components=tuple(components.values()),
            instances=tuple(instances),
            edges=tuple(edges),
            issues=tuple(issues),
        )


@dataclass(frozen=True, slots=True)
class _Ref:
    name: str
    source_kind: SourceKind
    source_key: str
    vcs_url: str | None = None
    commit: str | None = None
    qualifiers: tuple[tuple[str, str], ...] = ()

    @property
    def purl(self) -> str:
        return purl(Ecosystem.GENERIC, self.name, qualifiers=dict(self.qualifiers))

    @property
    def component(self) -> str:
        return self.purl

    @property
    def key(self) -> str:
        return self.source_key

    @property
    def instance_id(self) -> str:
        return f"{self.purl}#"


@dataclass(frozen=True, slots=True)
class _Command:
    name: str
    arguments: str
    line: int


def _resolve_command(
    command: _Command,
    path: str,
    vendored: bool,
    issues: list[Issue],
) -> tuple[tuple[_Ref, Scope, Resolution, str | None], ...]:
    tokens = _tokens(command.arguments)
    name = command.name.casefold()
    if name == "find_package":
        resolved = _find_package(tokens, command, path, vendored, issues)
        return (resolved,) if resolved is not None else ()
    if name == "pkg_check_modules":
        return _pkg_check_modules(tokens, command, path, vendored, issues)
    if name in {"fetchcontent_declare", "externalproject_add"}:
        resolved = _declare(tokens, command, path, vendored, issues)
        return (resolved,) if resolved is not None else ()
    return ()


def _find_package(
    tokens: Sequence[str],
    command: _Command,
    path: str,
    vendored: bool,
    issues: list[Issue],
) -> tuple[_Ref, Scope, Resolution, str | None] | None:
    if not tokens:
        return None
    package = tokens[0]
    if "$" in package or not package:
        _dynamic(path, command, issues, f"find_package name {package!r} is not literal")
        return None
    version = next(
        (
            token
            for token in tokens[1:]
            if token not in _PACKAGE_KEYWORDS
            and token[:1].isdigit()
            and not token.isupper()
        ),
        None,
    )
    if version is not None and "$" in version:
        _dynamic(path, command, issues, f"find_package version {version!r} is not literal")
        version = None
    ref = _Ref(
        package,
        SourceKind.VENDORED if vendored else SourceKind.REGISTRY,
        f"cmake:{package}",
    )
    return ref, Scope.SYSTEM, Resolution.DECLARED_ONLY, version


def _pkg_check_modules(
    tokens: Sequence[str],
    command: _Command,
    path: str,
    vendored: bool,
    issues: list[Issue],
) -> tuple[tuple[_Ref, Scope, Resolution, str | None], ...]:
    packages = [token for token in tokens[1:] if token not in _PACKAGE_KEYWORDS]
    resolved: list[tuple[_Ref, Scope, Resolution, str | None]] = []
    for package in packages:
        version: str | None = None
        for separator in (">=", "<=", "=", ">", "<"):
            if separator in package:
                package, _, version = package.partition(separator)
                break
        if "$" in package or not package:
            _dynamic(path, command, issues, f"pkg_check_modules name {package!r} is not literal")
            continue
        if version is not None and "$" in version:
            _dynamic(path, command, issues, f"pkg_check_modules version {version!r} is not literal")
            version = None
        ref = _Ref(
            package,
            SourceKind.VENDORED if vendored else SourceKind.REGISTRY,
            f"cmake:{package}",
        )
        resolved.append((ref, Scope.SYSTEM, Resolution.DECLARED_ONLY, version))
    return tuple(resolved)


def _declare(
    tokens: Sequence[str],
    command: _Command,
    path: str,
    vendored: bool,
    issues: list[Issue],
) -> tuple[_Ref, Scope, Resolution, str | None] | None:
    if not tokens:
        return None
    name = tokens[0]
    if "$" in name or not name:
        _dynamic(path, command, issues, f"{command.name} name {name!r} is not literal")
        return None
    values: dict[str, str] = {}
    index = 1
    while index < len(tokens):
        token = tokens[index]
        if token in _DECLARE_KEYWORDS and index + 1 < len(tokens):
            values[token] = tokens[index + 1]
            index += 2
            continue
        index += 1
    url = values.get("URL")
    if url is not None:
        if "$" in url:
            _dynamic(path, command, issues, f"{name} URL is not literal")
            return None
        target = redact_url(url, drop_query_and_fragment=True)
        checksum = values.get("URL_HASH")
        if checksum is not None and "$" in checksum:
            _dynamic(path, command, issues, f"{name} URL_HASH is not literal")
            checksum = None
        ref = _Ref(
            name,
            SourceKind.VENDORED if vendored else SourceKind.URL,
            f"cmake:url:{target}",
            qualifiers=(("download_url", target),),
        )
        return ref, Scope.BUILD, Resolution.RESOLVED if checksum is not None else Resolution.DECLARED_ONLY, checksum
    repository = values.get("GIT_REPOSITORY")
    if repository is not None:
        if "$" in repository:
            _dynamic(path, command, issues, f"{name} GIT_REPOSITORY is not literal")
            return None
        vcs_url = redact_url(repository, drop_query_and_fragment=True)
        tag = values.get("GIT_TAG")
        if tag is not None and "$" in tag:
            _dynamic(path, command, issues, f"{name} GIT_TAG is not literal")
            tag = None
        commit = tag if tag is not None and _is_sha(tag) else None
        ref = _Ref(
            name,
            SourceKind.VENDORED if vendored else SourceKind.GIT,
            f"cmake:git:{vcs_url}",
            vcs_url=vcs_url,
            commit=commit,
            qualifiers=(("vcs_url", vcs_url),),
        )
        return ref, Scope.BUILD, Resolution.RESOLVED if commit is not None else Resolution.DECLARED_ONLY, tag
    _dynamic(path, command, issues, f"{name} declares no URL or GIT_REPOSITORY")
    return None


def _dynamic(path: str, command: _Command, issues: list[Issue], message: str) -> None:
    issues.append(Issue(CoverageKind.DYNAMIC_VALUE, message, path=path, locator=f"L{command.line}"))


def _is_sha(value: str) -> bool:
    return len(value) == 40 and all(character in "0123456789abcdef" for character in value)


def _commands(text: str) -> tuple[_Command, ...]:
    cleaned = _strip_comments(text)
    commands: list[_Command] = []
    index = 0
    line = 1
    while index < len(cleaned):
        character = cleaned[index]
        if character == "\n":
            line += 1
            index += 1
            continue
        if character.isalpha() or character == "_":
            start = index
            while index < len(cleaned) and (cleaned[index].isalnum() or cleaned[index] == "_"):
                index += 1
            name = cleaned[start:index]
            probe = index
            while probe < len(cleaned) and cleaned[probe] in " \t":
                probe += 1
            if probe < len(cleaned) and cleaned[probe] == "(":
                end = _matching(cleaned, probe + 1)
                if end is None:
                    break
                arguments = cleaned[probe + 1 : end]
                commands.append(_Command(name, arguments, line))
                line += cleaned.count("\n", probe, end)
                index = end + 1
                continue
            continue
        index += 1
    return tuple(commands)


def _strip_comments(text: str) -> str:
    characters = list(text)
    index = 0
    in_string = False
    escaped = False
    while index < len(text):
        character = text[index]
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
            index += 1
            continue
        if character == '"':
            in_string = True
            index += 1
            continue
        if character == "#":
            end = text.find("\n", index)
            if end < 0:
                end = len(text)
            for position in range(index, end):
                characters[position] = " "
            index = end
            continue
        index += 1
    return "".join(characters)


def _matching(text: str, start: int) -> int | None:
    depth = 1
    index = start
    in_string = False
    escaped = False
    while index < len(text):
        character = text[index]
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
        elif character == '"':
            in_string = True
        elif character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
            if depth == 0:
                return index
        index += 1
    return None


def _tokens(arguments: str) -> list[str]:
    tokens: list[str] = []
    current: list[str] = []
    depth = 0
    in_string = False
    escaped = False
    for character in arguments:
        if in_string:
            if escaped:
                current.append(character)
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
            else:
                current.append(character)
            continue
        if character == '"':
            in_string = True
        elif character == "(":
            depth += 1
            current.append(character)
        elif character == ")":
            depth -= 1
            current.append(character)
        elif character.isspace() and depth == 0:
            if current:
                tokens.append("".join(current))
                current = []
        else:
            current.append(character)
    if current:
        tokens.append("".join(current))
    return tokens
