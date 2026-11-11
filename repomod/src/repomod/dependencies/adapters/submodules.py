from __future__ import annotations

import posixpath
from collections.abc import Sequence
from dataclasses import dataclass
from typing import ClassVar
from urllib.parse import urlsplit

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


@dataclass(frozen=True, slots=True)
class SubmodulesAdapter:
    name: ClassVar[str] = "submodules"

    def matches(self, path: str) -> bool:
        return path == ".gitmodules"

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        projects: dict[str, Project] = {}
        drafts: dict[str, _Ref] = {}
        edges: list[Edge] = []
        for content in inputs:
            directory = posixpath.dirname(content.path) or "."
            project = Project(f"{directory}:generic", Ecosystem.GENERIC, directory)
            projects.setdefault(project.id, project)
            gitlinks = dict(content.gitlinks)
            for submodule in _parse(content.path, content.text, issues):
                commit = gitlinks.get(submodule.path)
                ref = _ref(submodule, content.vendored, commit)
                drafts.setdefault(ref.key, ref)
                edges.append(
                    Edge(
                        from_id=project.id,
                        to_id=ref.instance_id,
                        relation=Relation.DIRECT,
                        scope=Scope.BUILD,
                        optional=False,
                        constraint=submodule.branch,
                        resolved_version=None,
                        condition=None,
                        source_kind=ref.source_kind,
                        resolution=Resolution.RESOLVED
                        if commit is not None
                        else Resolution.DECLARED_ONLY,
                        evidence=(Evidence(content.path, f"L{submodule.line}"),),
                    )
                )
        components: dict[str, Component] = {}
        instances: dict[str, Instance] = {}
        for ref in drafts.values():
            component = Component(ref.component, ref.ecosystem, ref.name, ref.name)
            components.setdefault(component.purl, component)
            instances.setdefault(
                ref.instance_id,
                Instance(
                    id=ref.instance_id,
                    purl=ref.purl,
                    component=ref.component,
                    version=None,
                    source_kind=ref.source_kind,
                    vcs_url=ref.vcs_url,
                    commit=ref.commit,
                    first_party=ref.first_party,
                ),
            )
        return AdapterResult(
            projects=tuple(projects.values()),
            components=tuple(components.values()),
            instances=tuple(instances.values()),
            edges=tuple(edges),
            issues=tuple(issues),
        )


@dataclass(frozen=True, slots=True)
class _Ref:
    name: str
    ecosystem: Ecosystem
    source_kind: SourceKind
    source_key: str
    vcs_url: str | None
    commit: str | None
    first_party: bool
    qualifiers: tuple[tuple[str, str], ...]

    @property
    def purl(self) -> str:
        return purl(self.ecosystem, self.name, qualifiers=dict(self.qualifiers))

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
class _Submodule:
    name: str
    path: str
    url: str
    branch: str | None
    line: int


def _parse(path: str, text: str, issues: list[Issue]) -> tuple[_Submodule, ...]:
    sections: list[tuple[int, dict[str, str]]] = []
    current: dict[str, str] | None = None
    line = 1
    for number, raw in enumerate(text.splitlines(), start=1):
        stripped = raw.strip()
        if not stripped or stripped.startswith(("#", ";")):
            continue
        if stripped.startswith("[submodule "):
            if current is not None:
                sections.append((line, current))
            name = stripped.removeprefix("[submodule ").removesuffix("]").strip().strip('"')
            current = {"name": name}
            line = number
            continue
        if current is None:
            continue
        key, separator, value = stripped.partition("=")
        if not separator:
            continue
        key = key.strip().casefold()
        if key in {"path", "url", "branch"}:
            current[key] = value.strip().strip('"')
    if current is not None:
        sections.append((line, current))

    submodules: list[_Submodule] = []
    for section_line, section in sections:
        submodule_path = section.get("path")
        url = section.get("url")
        if (
            submodule_path is None
            or url is None
            or len(submodule_path) == 0
            or len(url) == 0
        ):
            issues.append(
                Issue(
                    CoverageKind.DYNAMIC_VALUE,
                    f"submodule {section.get('name', '?')} has no path or url",
                    path=path,
                    locator=f"L{section_line}",
                )
            )
            continue
        submodules.append(
            _Submodule(
                section.get("name", submodule_path),
                submodule_path,
                url,
                section.get("branch"),
                section_line,
            )
        )
    return tuple(submodules)


def _ref(submodule: _Submodule, vendored: bool, commit: str | None) -> _Ref:
    github = _github(submodule.url)
    if github is not None:
        name, url = github
        return _Ref(
            name,
            Ecosystem.GITHUB,
            SourceKind.VENDORED if vendored else SourceKind.GIT,
            f"github:{name}",
            url,
            commit,
            False,
            (),
        )
    url = redact_url(submodule.url, drop_query_and_fragment=True)
    name = url.rstrip("/").rsplit("/", 1)[-1].removesuffix(".git") or submodule.name
    return _Ref(
        name,
        Ecosystem.GENERIC,
        SourceKind.VENDORED if vendored else SourceKind.GIT,
        f"git:{url}",
        url,
        commit,
        False,
        (("vcs_url", url),),
    )


def _github(url: str) -> tuple[str, str] | None:
    value = url.strip()
    try:
        if value.startswith("git@github.com:"):
            path = value.removeprefix("git@github.com:")
        elif value.startswith(("ssh://", "https://", "http://")):
            parsed = urlsplit(value)
            if parsed.hostname is None or parsed.hostname.casefold() != "github.com":
                return None
            path = parsed.path.lstrip("/")
        else:
            return None
    except ValueError:
        return None
    path = path.removesuffix(".git").removesuffix("/")
    parts = path.split("/")
    if len(parts) != 2 or not all(parts):
        return None
    return f"{parts[0]}/{parts[1]}", f"https://github.com/{parts[0]}/{parts[1]}"
