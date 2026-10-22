from __future__ import annotations

import os
import re
import sys
import tempfile
import tomllib
from collections.abc import Mapping
from pathlib import Path

from repomod.types import Catalog, RepoId, Repository

PRIMARY_TAGS = frozenset(
    {"tool", "library", "app", "extension", "system", "build", "config", "data"}
)
PUBLISHING_TAGS = frozenset({"crates-io", "pypi", "npm"})
TAGS = PRIMARY_TAGS | PUBLISHING_TAGS
_IDENTIFIER = re.compile(r"[A-Za-z0-9_.-]+\Z")


class CatalogError(ValueError):
    pass


def validate_identifier(value: str, kind: str) -> str:
    if not _IDENTIFIER.fullmatch(value) or value in {".", ".."}:
        raise CatalogError(f"invalid {kind} {value!r}")
    return value


def _expand_root(value: str, environ: Mapping[str, str]) -> Path:
    configured = environ.get("REPOMOD_ROOT", value)
    home = environ.get("HOME")
    if home is not None and configured == "~":
        configured = home
    elif home is not None and configured.startswith("~/"):
        configured = home + configured[1:]
    expanded = Path(configured).expanduser()
    if not expanded.is_absolute():
        raise CatalogError("workspace root must resolve to an absolute path")
    return expanded.resolve(strict=False)


def load_catalog(path: Path, environ: dict[str, str] | None = None) -> Catalog:
    environment = os.environ if environ is None else environ
    try:
        parsed = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise CatalogError(f"cannot read catalog {path}: {error}") from error
    if set(parsed) - {"version", "workspace", "repos", "overrides"}:
        unknown = sorted(set(parsed) - {"version", "workspace", "repos", "overrides"})
        raise CatalogError(f"unknown catalog keys: {', '.join(unknown)}")
    if type(parsed.get("version")) is not int or parsed["version"] != 1:
        raise CatalogError("catalog version must be 1")
    workspace = parsed.get("workspace")
    if not isinstance(workspace, dict) or set(workspace) != {"root", "personal_owner"}:
        raise CatalogError("[workspace] must contain only root and personal_owner")
    if not isinstance(workspace["root"], str) or not isinstance(
        workspace["personal_owner"], str
    ):
        raise CatalogError("workspace root and personal_owner must be strings")
    personal_owner = validate_identifier(workspace["personal_owner"], "owner")
    raw_repos = parsed.get("repos")
    if not isinstance(raw_repos, dict) or not raw_repos:
        raise CatalogError("[repos] must contain at least one owner")

    owners: dict[str, str] = {}
    ids: dict[str, str] = {}
    repositories: list[Repository] = []
    for owner, raw_names in raw_repos.items():
        validate_identifier(owner, "owner")
        owner_key = owner.casefold()
        if owner_key in owners:
            raise CatalogError(f"duplicate owner ignoring case: {owner!r} and {owners[owner_key]!r}")
        owners[owner_key] = owner
        if not isinstance(raw_names, dict):
            raise CatalogError(f"repositories for {owner!r} must be a table")
        names: dict[str, str] = {}
        for name, raw_tags in raw_names.items():
            validate_identifier(name, "repository")
            name_key = name.casefold()
            if name_key in names:
                raise CatalogError(
                    f"duplicate repository ignoring case under {owner}: "
                    f"{name!r} and {names[name_key]!r}"
                )
            names[name_key] = name
            repo_id = RepoId(owner, name)
            id_key = str(repo_id).casefold()
            if id_key in ids:
                raise CatalogError(f"duplicate repository ignoring case: {repo_id}")
            ids[id_key] = str(repo_id)
            if not isinstance(raw_tags, list) or not raw_tags:
                raise CatalogError(f"tags for {repo_id} must be a non-empty array")
            tags = [tag for tag in raw_tags if isinstance(tag, str)]
            if len(tags) != len(raw_tags):
                raise CatalogError(f"tags for {repo_id} must be strings")
            if len(set(tags)) != len(tags):
                raise CatalogError(f"duplicate tags for {repo_id}")
            unknown_tags = set(tags) - TAGS
            if unknown_tags:
                raise CatalogError(f"unknown tags for {repo_id}: {', '.join(sorted(unknown_tags))}")
            if not set(tags) & PRIMARY_TAGS:
                raise CatalogError(f"{repo_id} must have at least one primary tag")
            repositories.append(Repository(repo_id, tuple(tags)))

    overrides = parsed.get("overrides", {})
    if not isinstance(overrides, dict):
        raise CatalogError("[overrides] must be a table")
    repositories_by_id = {str(repo.id).casefold(): repo for repo in repositories}
    for key, raw_override in overrides.items():
        try:
            repo_id = RepoId.parse(key)
        except ValueError as error:
            raise CatalogError(str(error)) from error
        validate_identifier(repo_id.owner, "owner")
        validate_identifier(repo_id.name, "repository")
        repo = repositories_by_id.get(str(repo_id).casefold())
        if repo is None:
            raise CatalogError(f"override refers to uncatalogued repository {repo_id}")
        if not isinstance(raw_override, dict) or set(raw_override) != {"autofix", "reason"}:
            raise CatalogError(f"override for {repo_id} must contain autofix and reason")
        if raw_override["autofix"] is not False or not isinstance(raw_override["reason"], str):
            raise CatalogError(f"override for {repo_id} must disable autofix with a reason")
        reason = raw_override["reason"].strip()
        if not reason:
            raise CatalogError(f"override for {repo_id} needs a non-empty reason")
        index = repositories.index(repo)
        repositories[index] = Repository(repo.id, repo.tags, False, reason)

    root = _expand_root(workspace["root"], environment)
    _validate_checkout_collisions(root, personal_owner, repositories)
    repositories.sort(key=lambda item: (item.id.owner.casefold(), item.id.name.casefold()))
    return Catalog(path.resolve(), root, workspace["root"], personal_owner, tuple(repositories))


def _relative_parts(repo: Repository, personal_owner: str) -> tuple[str, ...]:
    if repo.id.owner.casefold() == personal_owner.casefold():
        return (repo.id.name,)
    return (repo.id.owner, repo.id.name)


def _validate_checkout_collisions(
    root: Path, personal_owner: str, repositories: list[Repository]
) -> None:
    seen: dict[tuple[str, ...], tuple[str, ...]] = {}
    for repo in repositories:
        parts = _relative_parts(repo, personal_owner)
        key = tuple(part.casefold() for part in parts)
        previous = seen.get(key)
        if previous is not None and previous != parts:
            raise CatalogError(f"checkout path collision ignoring case: {'/'.join(previous)} and {'/'.join(parts)}")
        for existing_key, existing_parts in seen.items():
            if existing_parts != parts and (
                key[: len(existing_key)] == existing_key or existing_key[: len(key)] == key
            ):
                raise CatalogError(
                    f"checkout path overlaps another repository: {'/'.join(existing_parts)} and {'/'.join(parts)}"
                )
        seen[key] = parts
        if sys.platform == "darwin":
            current = root
            for part in parts:
                if not current.is_dir():
                    break
                matches = [entry.name for entry in current.iterdir() if entry.name.casefold() == part.casefold()]
                if matches and part not in matches:
                    raise CatalogError(
                        f"checkout path collides ignoring case with existing {current / matches[0]}"
                    )
                current = current / part


def repository_path(catalog: Catalog, repo: Repository) -> Path:
    path = catalog.root.joinpath(*_relative_parts(repo, catalog.personal_owner))
    current = catalog.root
    for part in _relative_parts(repo, catalog.personal_owner):
        current = current / part
        if current.is_symlink():
            raise CatalogError(f"checkout path contains a symlink: {current}")
    if not path.resolve(strict=False).is_relative_to(catalog.root):
        raise CatalogError(f"checkout path escapes configured root: {repo.id}")
    if sys.platform == "darwin":
        _validate_checkout_collisions(catalog.root, catalog.personal_owner, [repo])
    return path


def find_repository(catalog: Catalog, value: str | None) -> Repository:
    if value is None:
        raise CatalogError("missing repository")
    if "/" in value:
        wanted = str(RepoId.parse(value)).casefold()
        matches = [repo for repo in catalog.repositories if str(repo.id).casefold() == wanted]
    else:
        validate_identifier(value, "repository")
        matches = [repo for repo in catalog.repositories if repo.id.name.casefold() == value.casefold()]
    if not matches:
        raise CatalogError(f"repository is not catalogued: {value}")
    if len(matches) > 1:
        names = ", ".join(str(repo.id) for repo in matches)
        raise CatalogError(f"ambiguous repository {value!r}: {names}")
    return matches[0]


def add_repository(catalog: Catalog, owner: str, name: str, tags: list[str]) -> bool:
    validate_identifier(owner, "owner")
    validate_identifier(name, "repository")
    if not tags or len(tags) != len(set(tags)):
        raise CatalogError("add requires one or more unique tags")
    if any(tag not in TAGS for tag in tags):
        raise CatalogError(f"unknown tags: {', '.join(sorted(set(tags) - TAGS))}")
    if not set(tags) & PRIMARY_TAGS:
        raise CatalogError("add requires at least one primary tag")
    repo_id = RepoId(owner, name)
    if any(str(repo.id).casefold() == str(repo_id).casefold() for repo in catalog.repositories):
        return False
    owners = {repo.id.owner.casefold(): repo.id.owner for repo in catalog.repositories}
    owner = owners.get(owner.casefold(), owner)
    repo_id = RepoId(owner, name)
    new_repo = Repository(repo_id, tuple(tags))
    repositories = [*catalog.repositories, new_repo]
    _validate_checkout_collisions(catalog.root, catalog.personal_owner, repositories)
    original = catalog.path.read_text(encoding="utf-8")
    owner_header = f'[repos."{_toml_string(owner)}"]'
    owner_match = re.search(rf"(?m)^{re.escape(owner_header)}\s*$", original)
    tags_value = ", ".join(f'"{_toml_string(tag)}"' for tag in tags)
    entry = f"{name} = [{tags_value}]"
    if owner_match:
        next_header = re.search(r"(?m)^\[", original[owner_match.end() :])
        insertion = owner_match.end() + next_header.start() if next_header else len(original)
        prefix = original[:insertion].rstrip("\n")
        suffix = original[insertion:]
        updated = f"{prefix}\n{entry}\n{suffix.lstrip(chr(10))}"
    else:
        updated = f"{original.rstrip()}\n\n{owner_header}\n{entry}\n"
    try:
        tomllib.loads(updated)
    except tomllib.TOMLDecodeError as error:
        raise CatalogError(f"adding {repo_id} would make catalog invalid: {error}") from error
    atomic_write(catalog.path, updated)
    return True


def atomic_write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temp_path = Path(temp_name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temp_path, path)
    except BaseException:
        temp_path.unlink(missing_ok=True)
        raise


def _toml_string(value: str) -> str:
    return value.replace("\\", "\\\\").replace('"', '\\"')
