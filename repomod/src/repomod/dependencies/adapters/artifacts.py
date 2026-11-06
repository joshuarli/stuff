from __future__ import annotations

from dataclasses import dataclass

_HEX = frozenset("0123456789abcdefABCDEF")
_DELIMITERS = frozenset(" \t\r\n\"'`\\;|&<>()[]")


@dataclass(frozen=True, slots=True)
class Artifact:
    url: str
    pinned: bool
    dynamic: bool


def logical_commands(text: str) -> tuple[tuple[str, int], ...]:
    commands: list[tuple[str, int]] = []
    buffer: list[str] = []
    start = 1
    for number, raw in enumerate(text.splitlines(), start=1):
        if not buffer:
            start = number
        stripped = raw.rstrip()
        if stripped.endswith("\\"):
            buffer.append(stripped[:-1])
            continue
        buffer.append(stripped)
        commands.append((" ".join(part.strip() for part in buffer), start))
        buffer = []
    if buffer:
        commands.append((" ".join(part.strip() for part in buffer), start))
    return tuple(commands)


def find_artifacts(command: str) -> tuple[Artifact, ...]:
    artifacts: list[Artifact] = []
    seen: set[str] = set()
    index = 0
    while True:
        index = command.find("https://", index)
        if index < 0:
            break
        end = index
        while end < len(command) and command[end] not in _DELIMITERS:
            end += 1
        url = command[index:end].rstrip(".,:")
        if url and url not in seen:
            seen.add(url)
            artifacts.append(Artifact(url, _pinned(command), "$" in url))
        index = max(end, index + 1)
    return tuple(artifacts)


def _pinned(command: str) -> bool:
    for index in range(max(0, len(command) - 63)):
        candidate = command[index : index + 64]
        if len(candidate) != 64 or not all(character in _HEX for character in candidate):
            continue
        before = command[index - 1] if index else ""
        after = command[index + 64] if index + 64 < len(command) else ""
        if before not in _HEX and after not in _HEX:
            return True
    return False
