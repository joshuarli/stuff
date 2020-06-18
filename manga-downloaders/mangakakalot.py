#!/usr/bin/env python

import re
import sys
from pathlib import Path

import httpx
from bs4 import BeautifulSoup

def tryint(s):
    try:
        return int(s)
    except ValueError:
        return s

def alphanum_key(s):
    """ Turn a string into a list of string and number chunks.
        "z23a" -> ["z", 23, "a"]
    """
    return [tryint(c) for c in re.split('([0-9]+)', s)]


http = httpx.Client()

page = "https://mangakakalot.com/manga/itsuyasan"
#page = "https://mangakakalot.com/read-dl3ii158504902435"
#page = "https://mangakakalot.com/manga/waltz_oshimi_shuzo"
r = http.get(page)
r.raise_for_status()

title_soup = BeautifulSoup(r.content, 'html.parser')

title = title_soup.find(class_="manga-info-text").find("h1").text
if not title:
    exit("Couldn't parse title.")

print(f"Getting chapter list for {title}")
title_p = Path(title)
title_p.mkdir(exist_ok=True)

# TODO: how to properly iterate through nested divs?
# The most correct method here would be to look at all the row divs
# nested inside the "chapter-list" div.
chapter_divs = title_soup.find_all("div", {"class": "row"})[1:]
chapters = []
for d in chapter_divs:
    a = d.find("a")

    reg = re.compile(r"[\w:]* (\d+[\.\d]*)")
    result = reg.search(a.text)
    if result is None:
        continue

    c = result.group(1)
    chapters.append((c, a["href"]))

chapters.sort(key=lambda x: alphanum_key(x[0]))

for c, link in chapters:
    c_p = title_p / c
    c_p.mkdir(exist_ok=True)

    print(f"Getting pages for chapter {c}")
    r = http.get(link)
    r.raise_for_status()

    chapter_soup = BeautifulSoup(r.content, 'html.parser')
    reader = chapter_soup.find(id="vungdoc")
    pages = [image["src"] for image in reader.find_all("img")]

    for p in pages:
        fn = p.split("/")[-1]
        fp = c_p / fn
        if fp.is_file():
            print(f"{fp} already exists; skipping.")
            continue

        print(f"Downloading page {fn} ({p})")
        r = http.get(p)
        r.raise_for_status()

        with fp.open(mode="wb") as f:
            f.write(r.content)
#            with http.stream("GET", p) as r:
#                for chunk in r.iter_bytes():
#                    f.write(chunk)
