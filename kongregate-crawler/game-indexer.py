#!/usr/bin/env python

import re
import httpx
from bs4 import BeautifulSoup

game_div = re.compile(r"^recommended_game_(?P<game_id>\d+)$")
game_link = re.compile(r"https?://www.kongregate.com/games/(?P<game_path>.*)$")

c = httpx.Client()
f = open("game_metadata.txt", "wt")

# TODO: atexit and signal handling for c.close(), f.close()

page = "https://www.kongregate.com/games?sort=gameplays"

# TODO: this is currently oneshot with no resuming or better data writing or whatever other proper features.
# For now I just want to collect a few game_id -> game_paths to download SWFs.

while True:
    print(f"fetching {page}")
    r = c.get(page)
    r.raise_for_status()

    soup = BeautifulSoup(r.content, 'html.parser')

    games = soup.find_all("div", {"id": game_div})
    for game in games:
        m = game_div.match(game["id"])
        if m is None:
            raise RuntimeError("failed to match game_div")
        game_id = m.groupdict()["game_id"]

        m = game_link.match(game.find("a")["href"])
        if m is None:
            raise RuntimeError("failed to match game_link")
        game_path = m.groupdict()["game_path"]

        print(f"found id {game_id} -> {game_path}")
        # this should be flushed but i don't care right now
        f.write(f"{game_id} {game_path}\n")

    li = soup.find('li', {"class": "next"})
    next_page = li.find("a")["href"]
    page = f"https://www.kongregate.com{next_page}"
