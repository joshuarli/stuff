from __future__ import annotations

from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from enum import Enum
from typing import ClassVar, Literal, Protocol
from urllib.parse import quote, urlsplit, urlunsplit

SCHEMA_VERSION = 1

type CoverageSeverity = Literal["error", "gap"]


class Ecosystem(Enum):
    CARGO = "cargo"
    PYPI = "pypi"
    NPM = "npm"
    GOLANG = "golang"
    SWIFT = "swift"
    GITHUB = "github"
    DOCKER = "docker"
    APK = "apk"
    GENERIC = "generic"


class CoverageKind(Enum):
    CHECKOUT_MISSING = "checkout_missing"
    ORIGIN_MISMATCH = "origin_mismatch"
    DEFAULT_REF_MISSING = "default_ref_missing"
    GIT_FAILURE = "git_failure"
    PARSE_ERROR = "parse_error"
    LOCK_MISMATCH = "lock_mismatch"
    MISSING_LOCK = "missing_lock"
    UNSUPPORTED_FORMAT = "unsupported_format"
    DYNAMIC_VALUE = "dynamic_value"
    SYMLINK_SKIPPED = "symlink_skipped"
    SUBMODULE = "submodule"
    VENDORED_UNKNOWN = "vendored_unknown"
    NON_UTF8 = "non_utf8"

    @property
    def severity(self) -> CoverageSeverity:
        return "error" if self in ERROR_COVERAGE_KINDS else "gap"


class Resolution(Enum):
    RESOLVED = "resolved"
    CONDITIONAL = "conditional"
    DECLARED_ONLY = "declared_only"
    UNKNOWN = "unknown"
    UNSUPPORTED = "unsupported"


class Relation(Enum):
    DIRECT = "direct"
    TRANSITIVE = "transitive"


class Scope(Enum):
    RUNTIME = "runtime"
    DEV = "dev"
    BUILD = "build"
    TEST = "test"
    OPTIONAL = "optional"
    PEER = "peer"
    CI = "ci"
    TOOLCHAIN = "toolchain"
    IMAGE = "image"
    SYSTEM = "system"
    SERVICE = "service"


class SourceKind(Enum):
    REGISTRY = "registry"
    GIT = "git"
    PATH = "path"
    WORKSPACE = "workspace"
    URL = "url"
    VENDORED = "vendored"
    IMAGE = "image"


ERROR_COVERAGE_KINDS = frozenset(
    {
        CoverageKind.CHECKOUT_MISSING,
        CoverageKind.ORIGIN_MISMATCH,
        CoverageKind.DEFAULT_REF_MISSING,
        CoverageKind.GIT_FAILURE,
        CoverageKind.PARSE_ERROR,
        CoverageKind.LOCK_MISMATCH,
    }
)
UNAVAILABLE_COVERAGE_KINDS = frozenset(
    {
        CoverageKind.CHECKOUT_MISSING,
        CoverageKind.ORIGIN_MISMATCH,
        CoverageKind.DEFAULT_REF_MISSING,
    }
)


class RepositoryState(Enum):
    COMPLETE = "complete"
    PARTIAL = "partial"
    FAILED = "failed"
    UNAVAILABLE = "unavailable"

    @classmethod
    def from_issues(cls, issues: Iterable[Issue]) -> RepositoryState:
        kinds = {issue.kind for issue in issues}
        if kinds & UNAVAILABLE_COVERAGE_KINDS:
            return cls.UNAVAILABLE
        if any(kind.severity == "error" for kind in kinds):
            return cls.FAILED
        if kinds:
            return cls.PARTIAL
        return cls.COMPLETE


@dataclass(frozen=True, slots=True, order=True)
class InputFile:
    path: str
    blob: str


@dataclass(frozen=True, slots=True)
class Evidence:
    path: str
    locator: str


@dataclass(frozen=True, slots=True)
class Project:
    id: str
    ecosystem: Ecosystem
    directory: str


@dataclass(frozen=True, slots=True)
class Component:
    purl: str
    ecosystem: Ecosystem
    name: str
    display_name: str


@dataclass(frozen=True, slots=True)
class Instance:
    id: str
    purl: str
    component: str
    version: str | None
    source_kind: SourceKind
    context: str = ""
    registry_url: str | None = None
    repository_url: str | None = None
    vcs_url: str | None = None
    commit: str | None = None
    first_party: bool = False


@dataclass(frozen=True, slots=True)
class Edge:
    from_id: str
    to_id: str
    relation: Relation
    scope: Scope
    optional: bool
    constraint: str | None
    resolved_version: str | None
    condition: str | None
    source_kind: SourceKind
    resolution: Resolution
    evidence: tuple[Evidence, ...] = ()


@dataclass(frozen=True, slots=True)
class Issue:
    kind: CoverageKind
    message: str
    path: str | None = None
    locator: str | None = None

    @property
    def severity(self) -> CoverageSeverity:
        return self.kind.severity


@dataclass(frozen=True, slots=True)
class RepositoryData:
    repository: str
    tags: tuple[str, ...]
    inputs: tuple[InputFile, ...]
    inputs_fingerprint: str
    projects: tuple[Project, ...] = ()
    components: tuple[Component, ...] = ()
    instances: tuple[Instance, ...] = ()
    edges: tuple[Edge, ...] = ()
    issues: tuple[Issue, ...] = ()
    schema: int = SCHEMA_VERSION

    @property
    def state(self) -> RepositoryState:
        return RepositoryState.from_issues(self.issues)


@dataclass(frozen=True, slots=True)
class IndexEntry:
    commit: str | None
    inputs_fingerprint: str
    state: RepositoryState


@dataclass(frozen=True, slots=True)
class Index:
    parser_version: int
    repositories: Mapping[str, IndexEntry]
    schema: int = SCHEMA_VERSION


@dataclass(frozen=True, slots=True)
class InputContent:
    path: str
    text: str
    vendored: bool = False
    gitlinks: tuple[tuple[str, str], ...] = ()


@dataclass(frozen=True, slots=True)
class AdapterResult:
    projects: tuple[Project, ...] = ()
    components: tuple[Component, ...] = ()
    instances: tuple[Instance, ...] = ()
    edges: tuple[Edge, ...] = ()
    issues: tuple[Issue, ...] = ()


class Adapter(Protocol):
    name: ClassVar[str]

    def matches(self, path: str) -> bool: ...

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult: ...


def purl(
    ecosystem: Ecosystem,
    name: str,
    version: str | None = None,
    *,
    qualifiers: Mapping[str, str] | None = None,
    subpath: str | None = None,
) -> str:
    if not name or name.startswith("/") or name.endswith("/"):
        raise ValueError(f"invalid package name: {name!r}")
    value = f"pkg:{ecosystem.value}/{'/'.join(quote(part, safe='') for part in name.split('/'))}"
    if version is not None:
        value += f"@{quote(version, safe='')}"
    pairs = sorted(qualifiers.items()) if qualifiers is not None else []
    if pairs:
        encoded = "&".join(f"{quote(key, safe='')}={quote(item, safe='')}" for key, item in pairs)
        value += f"?{encoded}"
    if subpath is not None and len(subpath) > 0:
        value += f"#{quote(subpath, safe='/')}"
    return value


def redact_url(value: str, *, drop_query_and_fragment: bool = False) -> str:
    try:
        parsed = urlsplit(value)
    except ValueError:
        return value
    netloc = parsed.netloc.rsplit("@", 1)[-1] if "@" in parsed.netloc else parsed.netloc
    query = "" if drop_query_and_fragment else parsed.query
    fragment = "" if drop_query_and_fragment else parsed.fragment
    return urlunsplit((parsed.scheme, netloc, parsed.path, query, fragment))
