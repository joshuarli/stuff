#!/usr/bin/env python

import asyncio
import uvloop
import httpx
from collections import defaultdict
from urllib.parse import urlparse


async def main(url, nworkers, jobq, resultq):
    u = urlparse(url)
    client = httpx.AsyncClient(base_url=f"{u.scheme}://{u.netloc}")

    for _ in range(nreqs):
        q_jobs.put_nowait(f"/{u.path}")

    # TODO: handle broken/closed TCP connections, they're currently fatal.

    async def worker():
        while True:
            try:
                path = jobq.get_nowait()
            except asyncio.QueueEmpty:
                return
            resp = await client.get(path)
            resultq.put_nowait({"status": resp.status_code})

    await asyncio.gather(*(worker() for _ in range(nworkers)))
    await client.aclose()


# TODO: if this is changed to localhost, then DNS somehow blocks stuff...
# like, you'll see bursts of # workers.
url = "http://127.0.0.1:8000"
workers = 100
nreqs = 1000
q_jobs = asyncio.Queue(maxsize=nreqs)  # url paths
q_results = asyncio.Queue(maxsize=nreqs)  # { status: code }

uvloop.install()
asyncio.run(main(url, workers, q_jobs, q_results))

statuses = defaultdict(int)
for _ in range(nreqs):
    result = q_results.get_nowait()
    status = result["status"]
    statuses[status] += 1

for code, freq in statuses.items():
    print(f"{code}: {freq}")
