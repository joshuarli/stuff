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
            textwrap.fill(f"({comment['by']}) {comment['text']}", width=80),
            "    " * indent,
        ),
        "\n",
    )
    if comment.get("kids"):
        for child in comment["kids"]:
            tprint(child, indent + 1)


with gzip.open(sys.argv[1], "rb") as f:
    story = json.load(f)

# NOTE: hackernews text contents appear to be in html, so would be nice to render as such in the future

print(f'{story["title"]} ({story["score"]})', end='\n\n')
if story["url"]:
    print(story["url"])
else:
    print(textwrap.fill(story["text"], width=80))

print(f"\n{'-' * 80}")

for comment in story["kids"]:
    tprint(comment)
