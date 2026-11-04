from dataclasses import dataclass

from .errors import InputError, IntegrityError
from .model import IndexDep, IndexEntry
from .names import validate_crate_name
from .requirements import canonical_requirement
from .semver import Version
from .wirejson import json_string

CRATES_IO_INDEX = "https://github.com/rust-lang/crates.io-index"


def native_config_json(public_base_url, auth_required):
    dl = f"{public_base_url.rstrip('/')}/blobs/{{sha256-checksum}}.crate"
    return (
        f'{{"dl":{json_string(dl)},"api":{json_string(public_base_url)},'
        f'"auth-required":{"true" if auth_required else "false"}}}'
    )


def mirror_config_json(public_base_url):
    dl = f"{public_base_url.rstrip('/')}/blobs/{{sha256-checksum}}.crate"
    return f'{{"dl":{json_string(dl)}}}'


def publish_to_index(body, packaged=None):
    validate_crate_name(body.name)
    Version.parse(body.vers)
    if packaged is not None:
        reconcile_manifest(body, packaged)

    deps = []
    for dep in body.deps:
        if not dep.name and dep.explicit_name_in_toml is None:
            raise InputError("dependency missing name")
        dep_name = dep.name or ""
        dep_package = dep.package
        if dep.explicit_name_in_toml is not None:
            dep_name = dep.explicit_name_in_toml
            if dep_package is None:
                dep_package = dep.name
        if not dep.version_req:
            raise InputError(f"dependency '{dep_name}' missing version_req")
        try:
            canonical_requirement(dep.version_req)
        except InputError as error:
            raise InputError(f"dependency '{dep_name}' has invalid version_req '{dep.version_req}'") from error
        if not dep_name:
            raise InputError("dependency with empty name")
        try:
            validate_crate_name(dep_name)
        except InputError as error:
            raise InputError(f"dependency has invalid name '{dep_name}'") from error
        if dep_package is not None:
            try:
                validate_crate_name(dep_package)
            except InputError as error:
                raise InputError(f"dependency has invalid package '{dep_package}'") from error
        deps.append(
            IndexDep(
                name=dep_name,
                req=dep.version_req,
                features=list(dep.features),
                optional=dep.optional,
                default_features=dep.default_features,
                target=dep.target,
                kind=dep.kind,
                registry=dep.registry or None,
                package=dep_package,
            )
        )

    uses_v2_syntax = any("dep:" in value or "?" in value for values in body.features.values() for value in values)
    return IndexEntry(
        name=body.name,
        vers=body.vers,
        deps=deps,
        cksum=body.checksum,
        features={key: list(values) for key, values in sorted(body.features.items())},
        yanked=body.yanked,
        links=body.links,
        rust_version=body.rust_version,
        v=2 if uses_v2_syntax else 1,
    )


@dataclass(frozen=True)
class DependencyKey:
    alias: str
    real: str
    kind: str
    target: str | None

    def sort_key(self):
        return (self.alias, self.real, self.kind, self.target is not None, self.target or "")


@dataclass
class NormalizedDep:
    key: DependencyKey
    version_req: str
    features: list
    optional: bool
    default_features: bool
    registry: str | None
    package: str | None


def normalize_target(target):
    return None if target is None else "".join(target.split())


def _registry(value):
    return value or None


def _publish_alias_real(dep):
    if dep.explicit_name_in_toml is not None:
        alias = dep.explicit_name_in_toml
        return alias, dep.package or dep.name or alias
    if dep.name is None:
        raise IntegrityError("publish dependency missing name")
    return dep.name, dep.package or dep.name


def reconcile_manifest(body, manifest):
    if manifest.name != body.name or manifest.version != body.vers:
        raise IntegrityError("name/version mismatch")
    if manifest.links != body.links:
        raise IntegrityError("links mismatch between archive and publish metadata")
    if manifest.rust_version != body.rust_version:
        raise IntegrityError("rust_version mismatch between archive and publish metadata")
    if not _features_equal(manifest.features, body.features):
        raise IntegrityError("features mismatch between archive and publish metadata")

    packaged = [
        NormalizedDep(
            DependencyKey(d.alias, d.package or d.alias, d.kind or "normal", normalize_target(d.target)),
            d.version_req,
            d.features,
            d.optional,
            d.default_features,
            _registry(d.registry),
            d.package,
        )
        for d in manifest.dependencies
    ]
    published = []
    for dep in body.deps:
        alias, real = _publish_alias_real(dep)
        published.append(
            (
                NormalizedDep(
                    DependencyKey(alias, real, dep.kind or "normal", normalize_target(dep.target)),
                    dep.version_req,
                    dep.features,
                    dep.optional,
                    dep.default_features,
                    _registry(dep.registry),
                    dep.package,
                ),
                dep,
            )
        )
    if len(packaged) != len(published):
        raise IntegrityError(
            f"dependency count mismatch: archive has {len(packaged)}, publish metadata has {len(published)}"
        )
    packaged_by_key = {}
    for index, dep in enumerate(packaged):
        packaged_by_key.setdefault(dep.key, []).append(index)
    published_by_key = {}
    for index, (dep, _) in enumerate(published):
        published_by_key.setdefault(dep.key, []).append(index)
    if sorted(packaged_by_key, key=DependencyKey.sort_key) != sorted(published_by_key, key=DependencyKey.sort_key):
        problems = [
            f"archive-only '{k.alias}' kind={k.kind} target={k.target!r}"
            for k in sorted(packaged_by_key, key=DependencyKey.sort_key)
            if k not in published_by_key
        ] + [
            f"publish-only '{k.alias}' kind={k.kind} target={k.target!r}"
            for k in sorted(published_by_key, key=DependencyKey.sort_key)
            if k not in packaged_by_key
        ]
        raise IntegrityError(f"dependency set mismatch (alias/package/kind/target): {'; '.join(problems)}")
    for key, packaged_indexes in packaged_by_key.items():
        published_indexes = published_by_key[key]
        if len(packaged_indexes) != len(published_indexes):
            raise IntegrityError(
                f"dependency '{key.alias}' count mismatch for kind={key.kind} target={key.target!r}"
            )
        for packaged_index, published_index in zip(packaged_indexes, published_indexes):
            expected = packaged[packaged_index]
            got, raw = published[published_index]
            if not _requirements_match(got.version_req, expected.version_req):
                raise IntegrityError(
                    f"dependency '{key.alias}' version_req mismatch: "
                    f"archive '{expected.version_req}' vs publish '{got.version_req}'"
                )
            if (
                sorted(got.features) != sorted(expected.features)
                or got.optional != expected.optional
                or got.default_features != expected.default_features
            ):
                raise IntegrityError(f"dependency '{key.alias}' flags mismatch")
            _match_registry(got.registry, expected.registry, key.alias)
            if not _package_matches(raw, expected.package, key.alias):
                raise IntegrityError(f"dependency '{key.alias}' package mismatch")


def _features_equal(a, b):
    return a.keys() == b.keys() and all(sorted(a[k]) == sorted(b[k]) for k in a)


def _match_registry(got, expected, alias):
    if got is None and expected is None:
        return
    if got is not None and expected is None and got == CRATES_IO_INDEX:
        return
    if got is None and expected is not None and expected != CRATES_IO_INDEX:
        return
    if got is not None and got == expected:
        if got == CRATES_IO_INDEX:
            return
        raise IntegrityError(f"dependency '{alias}' uses unsupported registry '{got}'")
    raise IntegrityError(f"dependency '{alias}' registry mismatch")


def _package_matches(raw, expected_package, alias):
    if raw.package == expected_package:
        return True
    if raw.package is None and expected_package == alias:
        return True
    if expected_package is None and raw.package == alias and raw.explicit_name_in_toml == alias:
        return True
    real = raw.package or raw.name or raw.explicit_name_in_toml
    return raw.package is None and expected_package is not None and real == expected_package


def _requirements_match(published, packaged):
    if published == packaged:
        return True
    try:
        return canonical_requirement(published) == canonical_requirement(packaged)
    except InputError:
        return False
