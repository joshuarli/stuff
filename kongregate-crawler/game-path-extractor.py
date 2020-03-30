#!/usr/bin/env python

import re
import httpx
from bs4 import BeautifulSoup

c = httpx.Client()

# XXX: this doesn't work, i get re.error when trying to match script.text.
# presumably because it contains regex metacharacters. oh well
# swf_link = re.compile(r"var swf_location = \"(?P<link>//chat\.kongregate\.com/gamez/[^\"]*)")

with open("game_metadata.txt", "rt") as f:
    games = f.readlines()

with open("swf_links.txt", "wt") as f:
    for game in games:
        game_id, game_path = game.split()
        print(f"fetching iframe for {game_path} ({game_id})")
        r = c.get(f"https://game{game_id}.konggames.com/games/{game_path}/frame")

        if r.status_code != 200:
            print(f"warning: failed with {r.status_code}")
            continue

        soup = BeautifulSoup(r.content, 'html.parser')
        for script in soup.find_all("script"):
            # HACK: swf_link doesn't work.
            fragments = script.text.split()
            try:
                i = fragments.index("swf_location")
            except ValueError:
                continue

            path = fragments[i+2][1:-2]  # strip leading " and trailing ";

            if ".html" in path:
                print(f"ignoring non-swf path {path}")
                continue

            if path.startswith("//"):
                path = "https:" + path

            print(path)
            f.write(path + "\n")
