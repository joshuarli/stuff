#!/usr/bin/env python3

import sys
import json
import gzip
import textwrap


def tprint(comment, indent=0):
    if not isinstance(comment, dict):
        return
    print(
        "\n"
        + textwrap.indent(
            textwrap.fill(f"({comment['score']}) {comment['text']}", width=80),
            "    " * indent,
        ),
        "\n",
    )
    if comment.get("replies"):
        for reply in comment["replies"]:
            tprint(reply, indent + 1)


data = None
with gzip.open(sys.argv[1], "rb") as f:
    data = json.load(f)

print(f'{data["post"]["url"]}\n')
print(data["post"].get("text", ''))

print(f"\n{'-' * 80}")

for c in data["replies"]:
    tprint(c)
