#!/usr/bin/env python3
"""Regenerate wheel_tags.json from packaging.tags (packaging 25.0).

Usage: uv run --with packaging==25.0 --python 3.15 python generate-wheel-tags.py > wheel_tags.json
"""

import json
import sys

from packaging.tags import compatible_tags, cpython_tags, mac_platforms

CASES = [
    ("3.15", "linux-x86_64", "manylinux_2_28"),
    ("3.15", "linux-aarch64", "manylinux_2_17"),
    ("3.15", "macos-arm64", "manylinux_2_28"),
    ("3.15", "macos-x86_64", "manylinux_2_28"),
    ("3.11", "linux-x86_64", "manylinux_2_5"),
]


def linux_platforms(arch: str, floor: int) -> list[str]:
    platforms = [f"manylinux_2_{minor}_{arch}" for minor in range(floor, 4, -1)]
    for minor, name in ((17, "manylinux2014"), (12, "manylinux2010"), (5, "manylinux1")):
        if floor >= minor:
            platforms.append(f"{name}_{arch}")
    return platforms


def main() -> None:
    cases = []
    for python, platform, policy in CASES:
        major, minor = map(int, python.split("."))
        arch = platform.split("-", 1)[1]
        if platform.startswith("macos-"):
            platforms = list(mac_platforms((15, 0), arch))
        else:
            platforms = linux_platforms(arch, int(policy.rsplit("_", 1)[1]))
        tags = [str(tag) for tag in cpython_tags((major, minor), abis=[f"cp{major}{minor}"], platforms=platforms)]
        tags += [str(tag) for tag in compatible_tags((major, minor), interpreter=f"cp{major}{minor}", platforms=platforms)]
        cases.append(
            {
                "python": python,
                "platform": platform,
                "policy": policy,
                "platforms": platforms,
                "tags": tags,
            }
        )
    json.dump({"packaging": "25.0", "cases": cases}, sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
