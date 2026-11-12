TARGETS = {"native": "x86_64-linux-musl"}


def native_target(platform: str) -> str:
    try:
        return TARGETS[platform]
    except KeyError as error:
        raise ValueError(f"unsupported target: {platform}") from error
