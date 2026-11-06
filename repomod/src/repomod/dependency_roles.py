from __future__ import annotations

import tomllib
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any, cast

from repomod.dependencies.model import Ecosystem

ROLES_PATH = Path(__file__).resolve().parents[2] / "dependency-roles.toml"
_KEYS = frozenset({"ecosystem", "preferred", "complements", "options", "exceptions", "ban"})


class BanMatch(Enum):
    EXACT = "exact"
    PREFIX = "prefix"


@dataclass(frozen=True, slots=True)
class Role:
    name: str
    ecosystem: Ecosystem
    preferred: str | None
    options: Mapping[str, frozenset[str]]
    complements: frozenset[str] = frozenset()
    exceptions: Mapping[str, str] = field(default_factory=dict)
    bans: Mapping[str, BanMatch] = field(default_factory=dict)

    def allows(self, chosen: frozenset[str]) -> bool:
        if self.preferred is None or self.preferred not in chosen:
            return False
        return chosen <= self.complements | {self.preferred}


@dataclass(frozen=True, slots=True)
class Ban:
    role: Role
    option: str
    replacement: str


def normalize(ecosystem: Ecosystem, name: str) -> str:
    if ecosystem is Ecosystem.CARGO:
        return name.replace("_", "-").casefold()
    return name.casefold()


def find_ban(roles: Iterable[Role], ecosystem: Ecosystem, name: str) -> Ban | None:
    wanted = normalize(ecosystem, name)
    for role in roles:
        if role.ecosystem is not ecosystem:
            continue
        for option, match in role.bans.items():
            for package in role.options[option]:
                listed = normalize(ecosystem, package)
                if wanted == listed or (
                    match is BanMatch.PREFIX and wanted.startswith(f"{listed}-")
                ):
                    return Ban(role, option, _replacement(role, ecosystem, wanted))
    return None


def load_roles(path: Path = ROLES_PATH) -> tuple[Role, ...]:
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"cannot read {path.name}: {error}") from error
    return parse_roles(document, path.name)


def parse_roles(document: Mapping[str, object], source: str) -> tuple[Role, ...]:
    roles = []
    for name, raw in document.items():
        if not isinstance(raw, dict):
            raise ValueError(f"{source} [{name}] must be a table")
        table = cast(Mapping[str, Any], raw)
        unknown = set(table) - _KEYS
        if unknown:
            raise ValueError(f"{source} [{name}] has unknown keys: {', '.join(sorted(unknown))}")
        try:
            ecosystem = Ecosystem(table.get("ecosystem"))
        except ValueError as error:
            raise ValueError(f"{source} [{name}] has an unknown ecosystem") from error
        options = _options(table.get("options"), name, source)
        preferred = table.get("preferred")
        if preferred is not None and preferred not in options:
            raise ValueError(f"{source} [{name}] prefers {preferred!r}, which is not an option")
        complements = table.get("complements", [])
        if not isinstance(complements, list) or not all(isinstance(item, str) for item in complements):
            raise ValueError(f"{source} [{name}] complements must list option names")
        if complements and preferred is None:
            raise ValueError(f"{source} [{name}] complements need a preferred option")
        if any(item == preferred or item not in options for item in complements):
            raise ValueError(f"{source} [{name}] complements must be other options of the role")
        exceptions = table.get("exceptions", {})
        if not isinstance(exceptions, dict) or not all(
            isinstance(reason, str) and reason.strip() for reason in exceptions.values()
        ):
            raise ValueError(f"{source} [{name}] exceptions must map repositories to reasons")
        roles.append(
            Role(
                name,
                ecosystem,
                cast(str | None, preferred),
                options,
                frozenset(cast(list[str], complements)),
                cast(dict[str, str], exceptions),
                _bans(table.get("ban", {}), name, source, options, preferred),
            )
        )
    return tuple(roles)


def _options(raw: object, name: str, source: str) -> dict[str, frozenset[str]]:
    if not isinstance(raw, dict) or len(raw) < 2:
        raise ValueError(f"{source} [{name}.options] must name at least two options")
    options: dict[str, frozenset[str]] = {}
    seen: dict[str, str] = {}
    for option, packages in cast(Mapping[str, object], raw).items():
        if (
            not isinstance(packages, list)
            or not packages
            or not all(isinstance(item, str) and item for item in packages)
        ):
            raise ValueError(f"{source} [{name}.options] {option} must list package names")
        for package in cast(list[str], packages):
            if package in seen:
                raise ValueError(
                    f"{source} [{name}] lists {package} under both {seen[package]} and {option}"
                )
            seen[package] = option
        options[option] = frozenset(cast(list[str], packages))
    return options


def _bans(
    raw: object,
    name: str,
    source: str,
    options: Mapping[str, frozenset[str]],
    preferred: object,
) -> dict[str, BanMatch]:
    if not isinstance(raw, dict):
        raise ValueError(f"{source} [{name}.ban] must map options to exact or prefix")
    if raw and preferred is None:
        raise ValueError(f"{source} [{name}] bans need a preferred option to name as the replacement")
    bans: dict[str, BanMatch] = {}
    for option, match in cast(Mapping[str, object], raw).items():
        if option not in options or option == preferred:
            raise ValueError(f"{source} [{name}.ban] {option} must be another option of the role")
        try:
            bans[option] = BanMatch(match)
        except ValueError as error:
            raise ValueError(f"{source} [{name}.ban] {option} must be exact or prefix") from error
    return bans


def _replacement(role: Role, ecosystem: Ecosystem, banned: str) -> str:
    preferred = cast(str, role.preferred)
    fork = f"laputa-{banned}"
    if any(normalize(ecosystem, package) == fork for package in role.options[preferred]):
        return f"{fork} from {preferred}"
    return preferred
