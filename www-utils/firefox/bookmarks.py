#!/usr/bin/env python3

import os
import sys
from pathlib import Path

ff_dir = Path(os.environ.get('FF_DIR', "~/.mozilla/firefox")).expanduser()

if len(sys.argv) < 2:
    print("no profile dir specified, naively autodetecting...", file=sys.stderr)
    from configparser import ConfigParser
    cfg = ConfigParser()
    cfg.read(ff_dir / 'profiles.ini')
    if not cfg.sections():
        exit(f"nonexistent file or not valid ini: {ff_dir / 'profiles.ini'}")
    profile_dir = ff_dir / cfg['Profile0']['Path']
else:
    profile_dir = sys.argv[1]

profile_path = ff_dir / profile_dir
if not profile_path.is_dir:
    exit(f"doesn't exist as a directory: {profile_path}")


import sqlite3
import atexit

conn = sqlite3.connect(profile_path / "places.sqlite")
atexit.register(conn.close)
c = conn.cursor()

urls = set(map(
    lambda x: x[0],
    filter(
        lambda x: x[0].startswith("http"),
        c.execute("select url from moz_places inner join moz_bookmarks on moz_places.id = moz_bookmarks.fk"),
    )
))

for url in urls:
    sys.stdout.write(f"{url}\n")
