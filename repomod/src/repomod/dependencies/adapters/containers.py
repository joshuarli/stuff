from __future__ import annotations

import posixpath
from collections.abc import Sequence
from dataclasses import dataclass
from typing import ClassVar

from repomod.dependencies.adapters.artifacts import find_artifacts, logical_commands
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

_DOCKER_HUB = frozenset({"docker.io", "index.docker.io", "registry-1.docker.io"})
_SHELL_OPERATORS = frozenset({"&&", "||", ";", "|", "&"})


@dataclass(frozen=True, slots=True)
class ContainersAdapter:
    name: ClassVar[str] = "containers"

    def matches(self, path: str) -> bool:
        name = path.rsplit("/", 1)[-1]
        return name == "Dockerfile" or name.startswith("Dockerfile.") or name.endswith(".Dockerfile")

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        projects: dict[str, Project] = {}
        drafts: dict[str, _Ref] = {}
        edges: list[Edge] = []
        for content in inputs:
            directory = posixpath.dirname(content.path) or "."
            project = Project(f"{directory}:generic", Ecosystem.GENERIC, directory)
            projects.setdefault(project.id, project)
            _dockerfile(content.path, content.text, content.vendored, project.id, drafts, edges, issues)
        components: dict[str, Component] = {}
        instances: list[Instance] = []
        for ref in drafts.values():
            component = Component(ref.component, ref.ecosystem, ref.name, ref.name)
            components.setdefault(component.purl, component)
            instances.append(
                Instance(
                    id=ref.instance_id,
                    purl=ref.purl,
                    component=ref.component,
                    version=ref.version,
                    source_kind=ref.source_kind,
                    commit=ref.commit,
                    first_party=ref.first_party,
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
    version: str | None
    ecosystem: Ecosystem
    source_kind: SourceKind
    qualifiers: tuple[tuple[str, str], ...] = ()
    commit: str | None = None
    first_party: bool = False

    @property
    def purl(self) -> str:
        return purl(self.ecosystem, self.name, self.version, qualifiers=dict(self.qualifiers))

    @property
    def component(self) -> str:
        return purl(self.ecosystem, self.name, qualifiers=dict(self.qualifiers))

    @property
    def instance_id(self) -> str:
        return f"{self.purl}#"


@dataclass(frozen=True, slots=True)
class Image:
    name: str
    tag: str | None
    digest: str | None
    qualifiers: tuple[tuple[str, str], ...]


def parse_image(value: str) -> Image | None:
    text = value.strip().strip("\"'")
    if not text:
        return None
    base, _, digest = text.partition("@")
    digest = digest or None
    last_slash = base.rfind("/")
    last_colon = base.rfind(":")
    if last_colon > last_slash:
        tag = base[last_colon + 1 :]
        path = base[:last_colon]
    else:
        tag = None
        path = base
    parts = path.split("/")
    if len(parts) > 1 and ("." in parts[0] or ":" in parts[0] or parts[0] == "localhost"):
        registry = parts[0]
        name = "/".join(parts[1:])
    else:
        registry = "docker.io"
        name = path
    if not name:
        return None
    qualifiers: list[tuple[str, str]] = []
    if digest is not None:
        qualifiers.append(("digest", digest))
    if registry not in _DOCKER_HUB:
        qualifiers.append(("repository_url", f"https://{registry}/"))
    return Image(name, tag or None, digest, tuple(qualifiers))


def image_ref(image: Image, source_kind: SourceKind = SourceKind.IMAGE) -> _Ref:
    return _Ref(
        image.name,
        image.tag,
        Ecosystem.DOCKER,
        source_kind,
        qualifiers=image.qualifiers,
    )


def _dockerfile(
    path: str,
    text: str,
    vendored: bool,
    project_id: str,
    drafts: dict[str, _Ref],
    edges: list[Edge],
    issues: list[Issue],
) -> None:
    arguments: dict[str, str] = {}
    aliases: set[str] = set()
    for command, line in logical_commands(text):
        stripped = command.strip()
        if not stripped or stripped.startswith("#"):
            continue
        instruction, _, rest = stripped.partition(" ")
        instruction = instruction.upper()
        rest = rest.strip()
        if instruction == "ARG":
            name, separator, value = rest.partition("=")
            if separator:
                arguments[name.strip()] = value.strip().strip("\"'")
            continue
        if instruction == "FROM":
            _from(rest, arguments, aliases, path, line, vendored, project_id, drafts, edges, issues)
            continue
        if instruction == "COPY":
            _copy_from(rest, arguments, aliases, path, line, vendored, project_id, drafts, edges, issues)
            continue
        if instruction == "RUN":
            substituted, _dynamic = _substitute(rest, arguments)
            _run(substituted, path, line, vendored, project_id, drafts, edges, issues)
    return


def _from(
    rest: str,
    arguments: dict[str, str],
    aliases: set[str],
    path: str,
    line: int,
    vendored: bool,
    project_id: str,
    drafts: dict[str, _Ref],
    edges: list[Edge],
    issues: list[Issue],
) -> None:
    fields = rest.split()
    while fields and fields[0].startswith("--"):
        fields.pop(0)
    if not fields:
        return
    image_text = fields[0]
    alias = fields[2] if len(fields) >= 3 and fields[1].casefold() == "as" else None
    substituted, dynamic = _substitute(image_text, arguments)
    if dynamic:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                f"FROM image {image_text!r} is not literal",
                path=path,
                locator=f"L{line}",
            )
        )
        if alias is not None:
            aliases.add(alias.casefold())
        return
    if substituted.casefold() in aliases or substituted.casefold() == "scratch":
        if alias is not None:
            aliases.add(alias.casefold())
        return
    image = parse_image(substituted)
    if alias is not None:
        aliases.add(alias.casefold())
    if image is None:
        return
    _image_edge(image, path, line, vendored, project_id, drafts, edges)


def _copy_from(
    rest: str,
    arguments: dict[str, str],
    aliases: set[str],
    path: str,
    line: int,
    vendored: bool,
    project_id: str,
    drafts: dict[str, _Ref],
    edges: list[Edge],
    issues: list[Issue],
) -> None:
    fields = rest.split()
    source = None
    for field in fields:
        if field.startswith("--from="):
            source = field.removeprefix("--from=")
            break
    if source is None:
        return
    source = source.strip("\"'")
    if source.isdigit() or source.casefold() in aliases:
        return
    substituted, dynamic = _substitute(source, arguments)
    if dynamic:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                f"COPY --from image {source!r} is not literal",
                path=path,
                locator=f"L{line}",
            )
        )
        return
    image = parse_image(substituted)
    if image is not None:
        _image_edge(image, path, line, vendored, project_id, drafts, edges)


def _image_edge(
    image: Image,
    path: str,
    line: int,
    vendored: bool,
    project_id: str,
    drafts: dict[str, _Ref],
    edges: list[Edge],
) -> None:
    ref = image_ref(
        image, SourceKind.VENDORED if vendored else SourceKind.IMAGE
    )
    drafts.setdefault(ref.instance_id, ref)
    edges.append(
        Edge(
            from_id=project_id,
            to_id=ref.instance_id,
            relation=Relation.DIRECT,
            scope=Scope.IMAGE,
            optional=False,
            constraint=image.digest or image.tag,
            resolved_version=image.tag,
            condition=None,
            source_kind=ref.source_kind,
            resolution=Resolution.RESOLVED if image.digest is not None else Resolution.DECLARED_ONLY,
            evidence=(Evidence(path, f"L{line}"),),
        )
    )


def _run(
    command: str,
    path: str,
    line: int,
    vendored: bool,
    project_id: str,
    drafts: dict[str, _Ref],
    edges: list[Edge],
    issues: list[Issue],
) -> None:
    words = _words(command)
    index = 0
    while index < len(words):
        if words[index] != "apk":
            index += 1
            continue
        probe = index + 1
        while probe < len(words) and words[probe].startswith("-"):
            probe += 1
        if probe >= len(words) or words[probe] != "add":
            index = probe + 1
            continue
        probe += 1
        while probe < len(words) and words[probe] not in _SHELL_OPERATORS:
            token = words[probe]
            probe += 1
            if token.startswith("-"):
                if token in {"--virtual", "-t"} and probe < len(words):
                    probe += 1
                continue
            if token.endswith(".apk"):
                continue
            name, separator, version = token.partition("=")
            if not name:
                continue
            if "$" in name:
                issues.append(
                    Issue(
                        CoverageKind.DYNAMIC_VALUE,
                        f"apk package {token!r} is not literal",
                        path=path,
                        locator=f"L{line}",
                    )
                )
                continue
            if separator and "$" in version:
                issues.append(
                    Issue(
                        CoverageKind.DYNAMIC_VALUE,
                        f"apk version for {name} is not literal",
                        path=path,
                        locator=f"L{line}",
                    )
                )
                version = ""
                separator = ""
            ref = _Ref(
                name,
                version if separator else None,
                Ecosystem.APK,
                SourceKind.VENDORED if vendored else SourceKind.REGISTRY,
            )
            drafts.setdefault(ref.instance_id, ref)
            edges.append(
                Edge(
                    from_id=project_id,
                    to_id=ref.instance_id,
                    relation=Relation.DIRECT,
                    scope=Scope.SYSTEM,
                    optional=False,
                    constraint=version if separator else None,
                    resolved_version=version if separator else None,
                    condition=None,
                    source_kind=ref.source_kind,
                    resolution=Resolution.RESOLVED if separator else Resolution.DECLARED_ONLY,
                    evidence=(Evidence(path, f"L{line}"),),
                )
            )
        index = probe
    for artifact in find_artifacts(command):
        if artifact.dynamic:
            issues.append(
                Issue(
                    CoverageKind.DYNAMIC_VALUE,
                    f"artifact URL {artifact.url!r} is not literal",
                    path=path,
                    locator=f"L{line}",
                )
            )
            continue
        ref = artifact_ref(artifact.url, vendored)
        drafts.setdefault(ref.instance_id, ref)
        edges.append(
            Edge(
                from_id=project_id,
                to_id=ref.instance_id,
                relation=Relation.DIRECT,
                scope=Scope.BUILD,
                optional=False,
                constraint=None,
                resolved_version=None,
                condition=None,
                source_kind=ref.source_kind,
                resolution=Resolution.RESOLVED if artifact.pinned else Resolution.DECLARED_ONLY,
                evidence=(Evidence(path, f"L{line}"),),
            )
        )


def artifact_ref(url: str, vendored: bool) -> _Ref:
    target = redact_url(url, drop_query_and_fragment=True)
    name = target.rstrip("/").rsplit("/", 1)[-1] or "artifact"
    return _Ref(
        name,
        None,
        Ecosystem.GENERIC,
        SourceKind.VENDORED if vendored else SourceKind.URL,
        qualifiers=(("download_url", target),),
    )


def _words(command: str) -> list[str]:
    words: list[str] = []
    current: list[str] = []
    quote: str | None = None
    index = 0
    while index < len(command):
        character = command[index]
        if quote is not None:
            if character == quote:
                quote = None
            else:
                current.append(character)
            index += 1
            continue
        if character in {"'", '"'}:
            quote = character
            index += 1
            continue
        if character.isspace():
            if current:
                words.append("".join(current))
                current = []
            index += 1
            continue
        if character in ";&|":
            if current:
                words.append("".join(current))
                current = []
            if index + 1 < len(command) and command[index + 1] == character and character in "&|":
                words.append(character * 2)
                index += 2
            else:
                words.append(character)
                index += 1
            continue
        current.append(character)
        index += 1
    if current:
        words.append("".join(current))
    return words


def _substitute(value: str, arguments: dict[str, str]) -> tuple[str, bool]:
    result: list[str] = []
    index = 0
    dynamic = False
    while index < len(value):
        if value.startswith("${", index):
            end = value.find("}", index)
            if end < 0:
                dynamic = True
                result.append(value[index:])
                break
            name = value[index + 2 : end]
            replacement = arguments.get(name)
            if replacement is None:
                dynamic = True
                result.append(value[index : end + 1])
            else:
                result.append(replacement)
            index = end + 1
            continue
        if value[index] == "$":
            end = index + 1
            while end < len(value) and (value[end].isalnum() or value[end] == "_"):
                end += 1
            name = value[index + 1 : end]
            if not name:
                result.append("$")
                index += 1
                continue
            replacement = arguments.get(name)
            if replacement is None:
                dynamic = True
                result.append(value[index:end])
            else:
                result.append(replacement)
            index = end
            continue
        result.append(value[index])
        index += 1
    return "".join(result), dynamic
