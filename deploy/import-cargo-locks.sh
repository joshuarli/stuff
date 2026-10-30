#!/bin/sh
# Import many Cargo.lock files into one mycrates state dir, one pin file per
# lockfile. The project name is the lockfile's directory path with "/" turned
# into "-", so nested lockfiles in one repository never collide.
#
# Usage: import-cargo-locks.sh STATE_DIR LOCKFILE...
# Optional env: MYCRATES_BIN=target/debug/mycrates
# Exits non-zero on the first lockfile that import-lock rejects. Re-running
# with the same lockfiles rewrites the same pin files.
set -eu
[ "$#" -ge 2 ] || { echo "usage: $0 STATE_DIR LOCKFILE..." >&2; exit 2; }
MYCRATES_BIN=${MYCRATES_BIN:-target/debug/mycrates}
STATE_DIR=$1
shift
for lock in "$@"; do
  dir=$(cd "$(dirname "$lock")" && pwd)
  project=$(printf '%s' "$dir" | sed 's|^/||; s|/|-|g')
  "$MYCRATES_BIN" import-lock "$lock" --project "$project" --state-dir "$STATE_DIR"
done
