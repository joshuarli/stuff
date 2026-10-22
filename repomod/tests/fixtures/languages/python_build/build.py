from buildsys import native_target


def build_target(platform: str) -> str:
    return native_target(platform)
