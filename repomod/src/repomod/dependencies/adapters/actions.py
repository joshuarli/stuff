from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any, ClassVar, cast

from ruamel.yaml import YAML
from ruamel.yaml.error import YAMLError

from repomod.dependencies.adapters.artifacts import find_artifacts, logical_commands
from repomod.dependencies.adapters.containers import Image, parse_image
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
class ActionsAdapter:
    name: ClassVar[str] = "actions"

    def matches(self, path: str) -> bool:
        workflow = path.removeprefix(".github/workflows/")
        return (
            path.startswith(".github/workflows/")
            and "/" not in workflow
            and workflow.endswith((".yml", ".yaml"))
        )

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        project = Project(".:generic", Ecosystem.GENERIC, ".")
        drafts: dict[str, _Ref] = {}
        edges: list[Edge] = []
        for content in inputs:
            try:
                yaml = YAML(typ="rt")
                yaml.version = (1, 2)
                document = yaml.load(content.text)
            except YAMLError as error:
                issues.append(
                    Issue(CoverageKind.PARSE_ERROR, f"invalid YAML: {error}", path=content.path)
                )
                continue
            if not isinstance(document, Mapping):
                issues.append(
                    Issue(CoverageKind.PARSE_ERROR, "workflow is not a mapping", path=content.path)
                )
                continue
            _workflow(
                document,
                content.path,
                content.vendored,
                project.id,
                drafts,
                edges,
                issues,
            )
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
                    vcs_url=ref.vcs_url,
                    commit=ref.commit,
                    first_party=ref.first_party,
                )
            )
        return AdapterResult(
            projects=(project,) if edges else (),
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
    source_key: str
    vcs_url: str | None = None
    commit: str | None = None
    first_party: bool = False
    subpath: str | None = None
    qualifiers: tuple[tuple[str, str], ...] = ()

    @property
    def purl(self) -> str:
        return purl(
            self.ecosystem,
            self.name,
            self.version,
            qualifiers=dict(self.qualifiers),
            subpath=self.subpath,
        )

    @property
    def component(self) -> str:
        return purl(self.ecosystem, self.name, qualifiers=dict(self.qualifiers), subpath=self.subpath)

    @property
    def key(self) -> str:
        return self.source_key

    @property
    def instance_id(self) -> str:
        return f"{self.purl}#"


def _workflow(
    document: Mapping[str, Any],
    path: str,
    vendored: bool,
    project_id: str,
    drafts: dict[str, _Ref],
    edges: list[Edge],
    issues: list[Issue],
) -> None:
    jobs = document.get("jobs")
    if not isinstance(jobs, Mapping):
        return
    for job_id, raw in jobs.items():
        if not isinstance(job_id, str) or not isinstance(raw, Mapping):
            continue
        job: Mapping[str, Any] = cast(Mapping[str, Any], raw)
        uses = job.get("uses")
        if isinstance(uses, str):
            comment = _comment(raw, "uses")
            _uses_edge(
                uses,
                comment,
                f"jobs.{job_id}.uses",
                path,
                vendored,
                project_id,
                drafts,
                edges,
                issues,
                Scope.CI,
            )
        container = job.get("container")
        _image_value(
            container,
            f"jobs.{job_id}.container",
            path,
            vendored,
            project_id,
            drafts,
            edges,
            issues,
        )
        services = job.get("services")
        if isinstance(services, Mapping):
            for name, service in services.items():
                if isinstance(name, str):
                    _image_value(
                        service,
                        f"jobs.{job_id}.services.{name}",
                        path,
                        vendored,
                        project_id,
                        drafts,
                        edges,
                        issues,
                    )
        steps = job.get("steps")
        if not isinstance(steps, list):
            continue
        for index, raw_step in enumerate(steps):
            if not isinstance(raw_step, Mapping):
                continue
            step: Mapping[str, Any] = cast(Mapping[str, Any], raw_step)
            step_uses = step.get("uses")
            if isinstance(step_uses, str):
                comment = _comment(raw_step, "uses")
                _uses_edge(
                    step_uses,
                    comment,
                    f"jobs.{job_id}.steps[{index}].uses",
                    path,
                    vendored,
                    project_id,
                    drafts,
                    edges,
                    issues,
                    Scope.CI,
                )
            run = step.get("run")
            if isinstance(run, str):
                _run_artifacts(
                    run,
                    f"jobs.{job_id}.steps[{index}].run",
                    path,
                    vendored,
                    project_id,
                    drafts,
                    edges,
                    issues,
                )


def _uses_edge(
    value: str,
    comment: str | None,
    locator: str,
    path: str,
    vendored: bool,
    project_id: str,
    drafts: dict[str, _Ref],
    edges: list[Edge],
    issues: list[Issue],
    scope: Scope,
) -> None:
    text = value.strip()
    if "${{" in text:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                "workflow reference is an expression",
                path=path,
                locator=locator,
            )
        )
        return
    if text.startswith("docker://"):
        image = parse_image(text.removeprefix("docker://"))
        if image is None:
            return
        ref = _image_ref(image, vendored)
        if ref.key not in drafts:
            drafts[ref.key] = ref
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
                resolution=Resolution.RESOLVED
                if image.digest is not None
                else Resolution.DECLARED_ONLY,
                evidence=(Evidence(path, locator),),
            )
        )
        return
    if text.startswith(("./", "../")):
        ref = _Ref(
            text.removeprefix("./"),
            None,
            Ecosystem.GENERIC,
            SourceKind.VENDORED if vendored else SourceKind.PATH,
            f"actions:path:{text}",
            first_party=True,
        )
        if ref.key not in drafts:
            drafts[ref.key] = ref
        edges.append(
            Edge(
                from_id=project_id,
                to_id=ref.instance_id,
                relation=Relation.DIRECT,
                scope=scope,
                optional=False,
                constraint=None,
                resolved_version=None,
                condition=None,
                source_kind=ref.source_kind,
                resolution=Resolution.RESOLVED,
                evidence=(Evidence(path, locator),),
            )
        )
        return
    reference, separator, ref_value = text.rpartition("@")
    parts = reference.split("/")
    if not separator or not ref_value or len(parts) < 2 or not all(parts[:2]):
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                f"workflow reference {value!r} cannot be parsed",
                path=path,
                locator=locator,
            )
        )
        return
    owner, repository = parts[0].casefold(), parts[1].casefold()
    subpath = "/".join(parts[2:]) or None
    commit = ref_value if _is_sha(ref_value) else None
    name = f"{owner}/{repository}"
    ref = _Ref(
        name,
        ref_value,
        Ecosystem.GITHUB,
        SourceKind.VENDORED if vendored else SourceKind.GIT,
        f"actions:{name}:{ref_value}",
        vcs_url=f"https://github.com/{name}",
        commit=commit,
        subpath=subpath,
        qualifiers=(),
    )
    if ref.key not in drafts:
        drafts[ref.key] = ref
    edges.append(
        Edge(
            from_id=project_id,
            to_id=ref.instance_id,
            relation=Relation.DIRECT,
            scope=scope,
            optional=False,
            constraint=comment or ref_value,
            resolved_version=ref_value,
            condition=None,
            source_kind=ref.source_kind,
            resolution=Resolution.RESOLVED if commit is not None else Resolution.DECLARED_ONLY,
            evidence=(Evidence(path, locator),),
        )
    )


def _image_value(
    value: object,
    locator: str,
    path: str,
    vendored: bool,
    project_id: str,
    drafts: dict[str, _Ref],
    edges: list[Edge],
    issues: list[Issue],
) -> None:
    if isinstance(value, Mapping):
        value = value.get("image")
    if not isinstance(value, str):
        return
    if "${{" in value:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                "container image is an expression",
                path=path,
                locator=locator,
            )
        )
        return
    image = parse_image(value)
    if image is None:
        return
    ref = _image_ref(image, vendored)
    if ref.key not in drafts:
        drafts[ref.key] = ref
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
            resolution=Resolution.RESOLVED
            if image.digest is not None
            else Resolution.DECLARED_ONLY,
            evidence=(Evidence(path, locator),),
        )
    )


def _run_artifacts(
    run: str,
    locator: str,
    path: str,
    vendored: bool,
    project_id: str,
    drafts: dict[str, _Ref],
    edges: list[Edge],
    issues: list[Issue],
) -> None:
    for command, _line in logical_commands(run):
        for artifact in find_artifacts(command):
            if artifact.dynamic:
                issues.append(
                    Issue(
                        CoverageKind.DYNAMIC_VALUE,
                        f"artifact URL {artifact.url!r} is not literal",
                        path=path,
                        locator=locator,
                    )
                )
                continue
            ref = _artifact_ref(artifact.url, vendored)
            if ref.key not in drafts:
                drafts[ref.key] = ref
            edges.append(
                Edge(
                    from_id=project_id,
                    to_id=ref.instance_id,
                    relation=Relation.DIRECT,
                    scope=Scope.CI,
                    optional=False,
                    constraint=None,
                    resolved_version=None,
                    condition=None,
                    source_kind=ref.source_kind,
                    resolution=Resolution.RESOLVED
                    if artifact.pinned
                    else Resolution.DECLARED_ONLY,
                    evidence=(Evidence(path, locator),),
                )
            )


def _comment(node: object, key: str) -> str | None:
    comments = getattr(node, "ca", None)
    if comments is None:
        return None
    items = getattr(comments, "items", None)
    if not isinstance(items, dict):
        return None
    tokens = items.get(key)
    if not isinstance(tokens, list) or len(tokens) < 3:
        return None
    token = tokens[2]
    value = cast(object, getattr(token, "value", None))
    if not isinstance(value, str):
        return None
    text = value.strip().lstrip("#").strip()
    return text or None


def _is_sha(value: str) -> bool:
    return len(value) == 40 and all(character in "0123456789abcdef" for character in value)


def _image_ref(image: Image, vendored: bool) -> _Ref:
    name = image.name
    tag = image.tag
    digest = image.digest
    qualifiers = image.qualifiers
    return _Ref(
        name,
        tag,
        Ecosystem.DOCKER,
        SourceKind.VENDORED if vendored else SourceKind.IMAGE,
        f"docker:{name}:{tag or ''}:{digest or ''}",
        qualifiers=qualifiers,
    )


def _artifact_ref(url: str, vendored: bool) -> _Ref:
    target = redact_url(url, drop_query_and_fragment=True)
    name = target.rstrip("/").rsplit("/", 1)[-1] or "artifact"
    return _Ref(
        name,
        None,
        Ecosystem.GENERIC,
        SourceKind.VENDORED if vendored else SourceKind.URL,
        f"url:{target}",
        qualifiers=(("download_url", target),),
    )
