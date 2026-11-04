from dataclasses import dataclass
from functools import cmp_to_key

from .errors import InputError

MAX_NUMERIC = 2**64 - 1
MAX_LENGTH = 256


def _numeric(text):
    if not text or not text.isascii() or not text.isdigit():
        return None
    if len(text) > 1 and text[0] == "0":
        return None
    value = int(text)
    return value if value <= MAX_NUMERIC else None


def _identifier_ok(text):
    return bool(text) and all(c.isascii() and (c.isalnum() or c == "-") for c in text)


@dataclass(frozen=True)
class Version:
    major: int
    minor: int
    patch: int
    pre: tuple = ()
    build: tuple = ()

    @classmethod
    def parse(cls, text):
        invalid = InputError(f"invalid version '{text}'")
        if not text or len(text) > MAX_LENGTH:
            raise invalid
        core_pre, plus, build_text = text.partition("+")
        build = ()
        if plus:
            build = tuple(build_text.split("."))
            if not build_text or not all(_identifier_ok(part) for part in build):
                raise invalid
        core, dash, pre_text = core_pre.partition("-")
        pieces = core.split(".")
        if len(pieces) != 3:
            raise invalid
        numbers = [_numeric(piece) for piece in pieces]
        if any(n is None for n in numbers):
            raise invalid
        pre = []
        if dash:
            if not pre_text:
                raise invalid
            for part in pre_text.split("."):
                if not _identifier_ok(part):
                    raise invalid
                if part.isdigit():
                    value = _numeric(part)
                    if value is None:
                        raise invalid
                    pre.append(value)
                else:
                    pre.append(part)
        return cls(numbers[0], numbers[1], numbers[2], tuple(pre), build)

    def __str__(self):
        text = f"{self.major}.{self.minor}.{self.patch}"
        if self.pre:
            text += "-" + ".".join(str(p) for p in self.pre)
        if self.build:
            text += "+" + ".".join(self.build)
        return text

    def uniqueness_key(self):
        return str(Version(self.major, self.minor, self.patch, self.pre))

    def precedence_key(self):
        if not self.pre:
            pre_key = (1,)
        else:
            pre_key = (0,) + tuple((0, p, "") if isinstance(p, int) else (1, 0, p) for p in self.pre)
        return (self.major, self.minor, self.patch, pre_key)

    def is_stable(self):
        return not self.pre


def compare_text(a, b):
    try:
        ka = Version.parse(a).precedence_key()
        kb = Version.parse(b).precedence_key()
    except InputError:
        return (a > b) - (a < b)
    return (ka > kb) - (ka < kb)


compare_key = cmp_to_key(compare_text)
