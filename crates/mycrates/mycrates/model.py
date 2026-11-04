from dataclasses import dataclass, field

from .errors import InputError
from .wirejson import compact, json_string

KNOWN_ENTRY_KEYS = frozenset(
    {"name", "vers", "deps", "cksum", "features", "yanked", "links", "rust_version", "v"}
)


def _expect(obj, key, kind, default, context):
    if key not in obj:
        return default
    value = obj[key]
    if kind is int:
        ok = isinstance(value, int) and not isinstance(value, bool)
    else:
        ok = isinstance(value, kind)
    if not ok:
        raise InputError(f"{context}.{key} has the wrong type")
    return value


def _optional_str(obj, key, context):
    if key not in obj or obj[key] is None:
        return None
    if not isinstance(obj[key], str):
        raise InputError(f"{context}.{key} has the wrong type")
    return obj[key]


def _string_list(obj, key, context):
    value = _expect(obj, key, list, [], context)
    if not all(isinstance(item, str) for item in value):
        raise InputError(f"{context}.{key} must be a list of strings")
    return list(value)


def _feature_map(value, context):
    if not isinstance(value, dict):
        raise InputError(f"{context} must be an object")
    out = {}
    for key, items in value.items():
        if not isinstance(items, list) or not all(isinstance(i, str) for i in items):
            raise InputError(f"{context}.{key} must be a list of strings")
        out[key] = list(items)
    return out


@dataclass
class IndexDep:
    name: str
    req: str
    features: list
    optional: bool
    default_features: bool
    target: str | None = None
    kind: str | None = None
    registry: str | None = None
    package: str | None = None

    @classmethod
    def from_obj(cls, obj):
        if not isinstance(obj, dict):
            raise InputError("index dependency must be an object")
        context = "dep"
        return cls(
            name=_expect(obj, "name", str, "", context),
            req=_expect(obj, "req", str, "", context),
            features=_string_list(obj, "features", context),
            optional=_expect(obj, "optional", bool, False, context),
            default_features=_expect(obj, "default_features", bool, False, context),
            target=_optional_str(obj, "target", context),
            kind=_optional_str(obj, "kind", context),
            registry=_optional_str(obj, "registry", context),
            package=_optional_str(obj, "package", context),
        )

    def to_obj(self):
        out = {
            "name": self.name,
            "req": self.req,
            "features": list(self.features),
            "optional": self.optional,
            "default_features": self.default_features,
        }
        if self.target is not None:
            out["target"] = self.target
        if self.kind is not None:
            out["kind"] = self.kind
        out["registry"] = self.registry
        if self.package is not None:
            out["package"] = self.package
        return out


@dataclass
class IndexEntry:
    name: str
    vers: str
    deps: list
    cksum: str
    features: dict
    yanked: bool = False
    links: str | None = None
    rust_version: str | None = None
    v: int | None = None
    extra: dict = field(default_factory=dict)

    @classmethod
    def from_obj(cls, obj):
        if not isinstance(obj, dict):
            raise InputError("index entry must be an object")
        context = "entry"
        deps = _expect(obj, "deps", list, [], context)
        return cls(
            name=_expect(obj, "name", str, "", context),
            vers=_expect(obj, "vers", str, "", context),
            deps=[IndexDep.from_obj(d) for d in deps],
            cksum=_expect(obj, "cksum", str, "", context),
            features=_feature_map(obj.get("features", {}), "entry.features"),
            yanked=_expect(obj, "yanked", bool, False, context),
            links=_optional_str(obj, "links", context),
            rust_version=_optional_str(obj, "rust_version", context),
            v=_optional_int(obj, "v", context),
            extra={k: v for k, v in obj.items() if k not in KNOWN_ENTRY_KEYS},
        )

    def to_obj(self):
        out = {
            "name": self.name,
            "vers": self.vers,
            "deps": [d.to_obj() for d in self.deps],
            "cksum": self.cksum,
            "features": {k: list(v) for k, v in self.features.items()},
            "yanked": self.yanked,
        }
        if self.links is not None:
            out["links"] = self.links
        if self.rust_version is not None:
            out["rust_version"] = self.rust_version
        if self.v is not None:
            out["v"] = self.v
        for key in sorted(self.extra):
            out[key] = self.extra[key]
        return out

    def index_line(self):
        parts = [f'"name":{json_string(self.name)}', f'"vers":{json_string(self.vers)}']
        deps = ",".join(compact(dep.to_obj()) for dep in self.deps)
        parts.append(f'"deps":[{deps}]')
        parts.append(f'"cksum":{json_string(self.cksum)}')
        parts.append(f'"features":{compact({k: list(v) for k, v in sorted(self.features.items())})}')
        parts.append(f'"yanked":{compact(self.yanked)}')
        if self.links is not None:
            parts.append(f'"links":{json_string(self.links)}')
        if self.rust_version is not None:
            parts.append(f'"rust_version":{json_string(self.rust_version)}')
        if self.v is not None:
            parts.append(f'"v":{self.v}')
        for key in sorted(self.extra):
            parts.append(f"{json_string(key)}:{compact(self.extra[key])}")
        return "{" + ",".join(parts) + "}"


def _optional_int(obj, key, context):
    if key not in obj or obj[key] is None:
        return None
    value = obj[key]
    if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value < 2**32:
        raise InputError(f"{context}.{key} has the wrong type")
    return value


@dataclass
class PublishDependency:
    name: str | None
    version_req: str
    features: list
    optional: bool
    default_features: bool
    target: str | None
    kind: str | None
    registry: str | None
    package: str | None
    explicit_name_in_toml: str | None

    @classmethod
    def from_obj(cls, obj):
        if not isinstance(obj, dict):
            raise InputError("publish dependency must be an object")
        context = "dep"
        return cls(
            name=_optional_str(obj, "name", context),
            version_req=_expect(obj, "version_req", str, "", context),
            features=_string_list(obj, "features", context),
            optional=_expect(obj, "optional", bool, False, context),
            default_features=_expect(obj, "default_features", bool, False, context),
            target=_optional_str(obj, "target", context),
            kind=_optional_str(obj, "kind", context),
            registry=_optional_str(obj, "registry", context),
            package=_optional_str(obj, "package", context),
            explicit_name_in_toml=_optional_str(obj, "explicit_name_in_toml", context),
        )


@dataclass
class PublishBody:
    name: str
    vers: str
    deps: list
    features: dict
    links: str | None
    rust_version: str | None
    checksum: str
    yanked: bool

    @classmethod
    def from_obj(cls, obj):
        if not isinstance(obj, dict):
            raise InputError("publish metadata must be an object")
        for required in ("name", "vers", "deps"):
            if required not in obj:
                raise InputError(f"publish metadata is missing {required}")
        context = "publish"
        name = _expect(obj, "name", str, "", context)
        vers = _expect(obj, "vers", str, "", context)
        deps = _expect(obj, "deps", list, [], context)
        return cls(
            name=name,
            vers=vers,
            deps=[PublishDependency.from_obj(d) for d in deps],
            features=_feature_map(obj.get("features", {}), "publish.features"),
            links=_optional_str(obj, "links", context),
            rust_version=_optional_str(obj, "rust_version", context),
            checksum=_expect(obj, "checksum", str, "", context),
            yanked=_expect(obj, "yanked", bool, False, context),
        )
