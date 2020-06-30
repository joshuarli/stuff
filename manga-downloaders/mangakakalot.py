#!/usr/bin/env python

import asyncio
import re
import os
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


async def main(*, page, workers):
    http = httpx.AsyncClient(
        headers={"Referer": "https://mangakakalot.com"},
        timeout=None
    )

    r = await http.get(page)
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

    q_chapter_jobs = asyncio.Queue()

    for c, link in chapters:
        c_p = title_p / c
        c_p.mkdir(exist_ok=True)
        q_chapter_jobs.put_nowait((c_p, link, c))

    q_image_jobs = asyncio.Queue()

    async def page_downloader_worker():
        while True:
            try:
                c_p, link, c = q_chapter_jobs.get_nowait()
            except asyncio.QueueEmpty:
                return

            print(f"Getting pages for chapter {c}")
            r = await http.get(link)
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

                # TODO: we should put this into a sorted list so image downloading is more natural
                q_image_jobs.put_nowait((p, fp))

    # q_chapter_jobs should not be written to at this point.
    # I looked and there weren't any methods to like, cap maxsize,
    # or "close" the queue to be readonly.
    await asyncio.gather(
        *(page_downloader_worker() for _ in range(workers))
    )

    async def image_downloader_worker():
        while True:
            try:
                p, fp = q_image_jobs.get_nowait()
            except asyncio.QueueEmpty:
                return

            print(f"Downloading {p}")
            r = await http.get(p)
            r.raise_for_status()  # TODO: let's not fatal here, or be more graceful?

            with fp.open(mode="wb") as f:
                f.write(r.content)
    #            with http.stream("GET", p) as r:
    #                for chunk in r.iter_bytes():
    #                    f.write(chunk)

    # q_image_jobs should not be written to at this point.
    await asyncio.gather(
        *(image_downloader_worker() for _ in range(workers))
    )
    await http.aclose()


num_workers = os.cpu_count()
if not num_workers:
    raise RuntimeError("Couldn't determine CPU count.")

page = "https://mangakakalot.com/manga/itsuyasan"
#page = "https://mangakakalot.com/read-dl3ii158504902435"
#page = "https://mangakakalot.com/manga/waltz_oshimi_shuzo"
asyncio.run(main(
    page=page,
    workers=num_workers,
))
