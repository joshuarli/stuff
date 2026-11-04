import re
from dataclasses import dataclass

from .errors import InputError
from .semver import MAX_NUMERIC

_COMPARATOR = re.compile(r"^(\^|~|=|>=|<=|>|<)?\s*(.+)$")
_PRERELEASE = re.compile(r"^[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*$")


@dataclass(frozen=True)
class Bound:
    version: tuple
    inclusive: bool


def _version_key(major, minor, patch, pre=()):
    pre_key = (1,) if not pre else (0,) + tuple((0, p, "") if isinstance(p, int) else (1, 0, p) for p in pre)
    return (major, minor, patch, pre_key)


def _parse_partial(text, original):
    bad = InputError(f"invalid version_req '{original}'")
    text = text.strip()
    if not text:
        raise bad
    text, _, _ = text.partition("+")
    core, dash, pre_text = text.partition("-")
    parts = core.split(".")
    if len(parts) > 3:
        raise bad
    numbers = []
    starred = False
    for part in parts:
        if part in ("*", "x", "X"):
            starred = True
            numbers.append(None)
            continue
        if not part.isascii() or not part.isdigit() or (len(part) > 1 and part[0] == "0"):
            raise bad
        value = int(part)
        if value > MAX_NUMERIC:
            raise bad
        numbers.append(value)
    for index in range(1, len(numbers)):
        if numbers[index] is not None and numbers[index - 1] is None:
            raise bad
    while len(numbers) < 3:
        numbers.append(None)
    pre = ()
    if dash:
        if numbers[2] is None or not _PRERELEASE.match(pre_text):
            raise bad
        pre = tuple(int(p) if p.isdigit() else p for p in pre_text.split("."))
    return numbers[0], numbers[1], numbers[2], pre, starred


def _comparator_interval(op, partial):
    major, minor, patch, pre, starred = partial
    if starred and op in (None, "="):
        op = "~"
    wildcard = major is None

    def floor():
        return Bound(_version_key(major or 0, minor or 0, patch or 0, pre), True)

    def next_after(level):
        if level == 0:
            return Bound(_version_key(major + 1, 0, 0), False)
        if level == 1:
            return Bound(_version_key(major, minor + 1, 0), False)
        return Bound(_version_key(major, minor, patch + 1), False)

    specified = 0 if major is None else 1 if minor is None else 2 if patch is None else 3
    if wildcard:
        if op in (None, "^", "=", "~", ">=", "<="):
            return None, None
        raise InputError("invalid version_req wildcard operator")
    if op == "=":
        if specified == 3:
            exact = Bound(_version_key(major, minor, patch, pre), True)
            return exact, exact
        return floor(), next_after(specified - 1)
    if op == ">":
        if specified == 3:
            return Bound(_version_key(major, minor, patch, pre), False), None
        return Bound(next_after(specified - 1).version, True), None
    if op == ">=":
        return floor(), None
    if op == "<":
        return None, Bound(floor().version, False)
    if op == "<=":
        if specified == 3:
            return None, Bound(_version_key(major, minor, patch, pre), True)
        return None, next_after(specified - 1)
    if op == "~":
        return floor(), next_after(0 if specified == 1 else 1)
    if specified == 1 or major != 0:
        return floor(), next_after(0)
    if specified == 2 or minor != 0:
        return floor(), next_after(1)
    return floor(), next_after(2)


def _tighter_lower(a, b):
    if a is None:
        return b
    if b is None:
        return a
    if a.version != b.version:
        return a if a.version > b.version else b
    return a if not a.inclusive else b


def _tighter_upper(a, b):
    if a is None:
        return b
    if b is None:
        return a
    if a.version != b.version:
        return a if a.version < b.version else b
    return a if not a.inclusive else b


def canonical_requirement(text):
    stripped = text.strip()
    if not stripped:
        raise InputError("empty version_req")
    lower = upper = None
    for part in stripped.split(","):
        part = part.strip()
        if not part:
            raise InputError(f"invalid version_req '{text}'")
        match = _COMPARATOR.match(part)
        op, rest = match.group(1), match.group(2)
        partial = _parse_partial(rest, text)
        low, high = _comparator_interval(op, partial)
        lower = _tighter_lower(lower, low)
        upper = _tighter_upper(upper, high)
    if lower is not None and upper is not None:
        if lower.version > upper.version or (
            lower.version == upper.version and not (lower.inclusive and upper.inclusive)
        ):
            return ("empty",)
    return (lower, upper)
