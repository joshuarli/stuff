from .errors import InputError

MAX_NAME_LENGTH = 64


def validate_crate_name(name):
    if not name:
        raise InputError("empty crate name")
    if not (name[0].isascii() and name[0].isalpha()):
        raise InputError("crate name must start with letter")
    if len(name) > MAX_NAME_LENGTH:
        raise InputError("crate name too long")
    for char in name:
        if not (char.isascii() and (char.isalnum() or char in "-_")):
            raise InputError("invalid crate name character")
    return name


def lower_name(name):
    return name.lower()


def normalized_key(name):
    return name.lower().replace("_", "-")


def shard_for_name(name_lower):
    if not name_lower.isascii():
        return None
    length = len(name_lower)
    if length == 0:
        return "1"
    if length == 1:
        return f"1/{name_lower}"
    if length == 2:
        return f"2/{name_lower}"
    if length == 3:
        return f"3/{name_lower[0]}/{name_lower}"
    return f"{name_lower[:2]}/{name_lower[2:4]}/{name_lower}"
