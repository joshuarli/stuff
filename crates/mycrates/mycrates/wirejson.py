import json

from .errors import InputError

_ESCAPES = {'"': '\\"', "\\": "\\\\", "\n": "\\n", "\r": "\\r", "\t": "\\t"}


def json_string(text):
    out = ['"']
    for char in text:
        escaped = _ESCAPES.get(char)
        if escaped is not None:
            out.append(escaped)
        elif ord(char) < 0x20:
            out.append(f"\\u{ord(char):04x}")
        else:
            out.append(char)
    out.append('"')
    return "".join(out)


def compact(value):
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, str):
        return json_string(value)
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        return json.dumps(value)
    if isinstance(value, (list, tuple)):
        return "[" + ",".join(compact(v) for v in value) + "]"
    if isinstance(value, dict):
        return "{" + ",".join(f"{json_string(k)}:{compact(v)}" for k, v in value.items()) + "}"
    raise TypeError(f"not a JSON value: {value!r}")


def _reject_constant(name):
    raise ValueError(f"invalid JSON constant {name}")


def loads(text):
    try:
        return json.loads(text, parse_constant=_reject_constant)
    except (ValueError, RecursionError) as error:
        raise InputError(str(error)) from error


def loads_bytes(data):
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise InputError(str(error)) from error
    return loads(text)


def pretty(value):
    return json.dumps(value, indent=2, ensure_ascii=False)
