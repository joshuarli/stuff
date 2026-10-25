from __future__ import annotations

import json
import os
import tomllib
from pathlib import Path
from typing import Any


def read_toml(path: Path) -> tuple[dict[str, Any] | None, str | None]:
    try:
        value = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        return None, str(error)
    return value, None


def strip_jsonc(source: str) -> tuple[str | None, str | None]:
    characters = list(source)
    in_string = False
    escaped = False
    index = 0
    while index < len(source):
        character = source[index]
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
        elif character == '"':
            in_string = True
        elif source.startswith("//", index):
            while index < len(source) and source[index] not in "\r\n":
                characters[index] = " "
                index += 1
            continue
        elif source.startswith("/*", index):
            start = index
            end = source.find("*/", index + 2)
            if end < 0:
                return None, f"unterminated block comment at offset {start}"
            for comment_index in range(index, end + 2):
                if source[comment_index] not in "\r\n":
                    characters[comment_index] = " "
            index = end + 2
            continue
        index += 1
    cleaned = "".join(characters)
    characters = list(cleaned)
    in_string = False
    escaped = False
    for index, character in enumerate(cleaned):
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
        elif character == '"':
            in_string = True
        elif character == ",":
            following = index + 1
            while following < len(cleaned) and cleaned[following].isspace():
                following += 1
            if following < len(cleaned) and cleaned[following] in "]}":
                characters[index] = " "
    return "".join(characters), None


def read_json_object(
    path: Path, *, allow_jsonc: bool = False
) -> tuple[dict[str, Any] | None, str | None]:
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        return None, str(error)
    if allow_jsonc:
        source, error = strip_jsonc(source)
        if source is None:
            return None, error
    try:
        value = json.loads(source)
    except json.JSONDecodeError as error:
        return None, str(error)
    if not isinstance(value, dict):
        return None, "top-level value must be an object"
    return value, None


def cache_file(name: str) -> Path:
    cache_root = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")).expanduser()
    return cache_root / "repomod" / name
