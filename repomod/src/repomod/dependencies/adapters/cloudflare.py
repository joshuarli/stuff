from __future__ import annotations

import json
import posixpath
import tomllib
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any, ClassVar, cast

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
)
from repomod.io import strip_jsonc


@dataclass(frozen=True, slots=True)
class CloudflareAdapter:
    name: ClassVar[str] = "cloudflare"

    def matches(self, path: str) -> bool:
        name = path.rsplit("/", 1)[-1]
        return name in {"wrangler.toml", "wrangler.json", "wrangler.jsonc"} or (
            name.startswith("wrangler.") and name.endswith(".jsonc")
        )

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        projects: dict[str, Project] = {}
        drafts: dict[str, _Ref] = {}
        edges: list[Edge] = []
        for content in inputs:
            document = _load(content.path, content.text, issues)
            if document is None:
                continue
            directory = posixpath.dirname(content.path) or "."
            bindings = _bindings(document)
            if not bindings:
                continue
            project = Project(f"{directory}:generic", Ecosystem.GENERIC, directory)
            projects.setdefault(project.id, project)
            for kind, binding, identifier, locator in bindings:
                ref = _Ref(f"cloudflare-{kind}/{binding}", SourceKind.REGISTRY)
                drafts.setdefault(ref.key, ref)
                edges.append(
                    Edge(
                        from_id=project.id,
                        to_id=ref.instance_id,
                        relation=Relation.DIRECT,
                        scope=Scope.SERVICE,
                        optional=False,
                        constraint=identifier,
                        resolved_version=None,
                        condition=None,
                        source_kind=ref.source_kind,
                        resolution=Resolution.RESOLVED,
                        evidence=(Evidence(content.path, locator),),
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

    @property
    def purl(self) -> str:
        return purl(Ecosystem.GENERIC, self.name)

    @property
    def component(self) -> str:
        return self.purl

    @property
    def key(self) -> str:
        return self.purl

    @property
    def instance_id(self) -> str:
        return f"{self.purl}#"


def _load(path: str, text: str, issues: list[Issue]) -> dict[str, Any] | None:
    name = path.rsplit("/", 1)[-1]
    try:
        if name.endswith(".toml"):
            document = tomllib.loads(text)
        elif name.endswith(".jsonc"):
            cleaned, error = strip_jsonc(text)
            if cleaned is None:
                issues.append(
                    Issue(CoverageKind.PARSE_ERROR, f"invalid JSONC: {error}", path=path)
                )
                return None
            document = json.loads(cleaned)
        else:
            document = json.loads(text)
    except (tomllib.TOMLDecodeError, json.JSONDecodeError) as error:
        issues.append(Issue(CoverageKind.PARSE_ERROR, f"invalid configuration: {error}", path=path))
        return None
    return cast(dict[str, Any], document) if isinstance(document, dict) else None


def _bindings(document: Mapping[str, Any]) -> tuple[tuple[str, str, str, str], ...]:
    bindings: list[tuple[str, str, str, str]] = []
    for config in _configs(document):
        bindings.extend(_config_bindings(config))
    return tuple(bindings)


def _configs(document: Mapping[str, Any]) -> tuple[Mapping[str, Any], ...]:
    configs = [document]
    environment = document.get("env")
    if isinstance(environment, dict):
        configs.extend(
            cast(Mapping[str, Any], value)
            for value in environment.values()
            if isinstance(value, dict)
        )
    return tuple(configs)


def _config_bindings(config: Mapping[str, Any]) -> list[tuple[str, str, str, str]]:
    bindings: list[tuple[str, str, str, str]] = []
    bindings.extend(
        _list_bindings(config, "d1_databases", "d1", "binding", ("database_id", "database_name"))
    )
    bindings.extend(_list_bindings(config, "kv_namespaces", "kv", "binding", ("id",)))
    bindings.extend(_list_bindings(config, "r2_buckets", "r2", "binding", ("bucket_name",)))
    bindings.extend(
        _list_bindings(config, "services", "service", "binding", ("service", "environment"))
    )
    durable = _table(config.get("durable_objects"))
    if durable is not None:
        bindings.extend(
            _list_bindings(
                durable,
                "bindings",
                "durable-object",
                "name",
                ("class_name",),
                "durable_objects.",
            )
        )
    queues = _table(config.get("queues"))
    if queues is not None:
        bindings.extend(
            _list_bindings(
                queues,
                "producers",
                "queue",
                "binding",
                ("queue",),
                "queues.",
            )
        )
        for index, raw in enumerate(_items(queues.get("consumers"))):
            table = _table(raw)
            if table is None:
                continue
            name = table.get("queue")
            if isinstance(name, str) and name:
                bindings.append(
                    ("queue", name, name, f"queues.consumers[{index}].queue")
                )
    return bindings


def _list_bindings(
    config: Mapping[str, Any],
    key: str,
    kind: str,
    name_key: str,
    identifier_keys: tuple[str, ...],
    prefix: str = "",
) -> list[tuple[str, str, str, str]]:
    bindings: list[tuple[str, str, str, str]] = []
    for index, raw in enumerate(_items(config.get(key))):
        table = _table(raw)
        if table is None:
            continue
        name = table.get(name_key)
        if not isinstance(name, str) or not name:
            continue
        identifier = next(
            (
                value
                for identifier_key in identifier_keys
                if isinstance((value := table.get(identifier_key)), str) and value
            ),
            None,
        )
        bindings.append((kind, name, identifier or "", f"{prefix}{key}[{index}].{name_key}"))
    return bindings


def _items(value: object) -> list[Any]:
    return cast(list[Any], value) if isinstance(value, list) else []


def _table(value: object) -> dict[str, Any] | None:
    return cast(dict[str, Any], value) if isinstance(value, dict) else None
