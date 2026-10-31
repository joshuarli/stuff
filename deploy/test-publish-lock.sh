#!/bin/sh
# Check that deploy/publish-lock.sh admits one writer at a time, including
# while reclaiming a stale directory lock left by an older release.
# Usage: deploy/test-publish-lock.sh [WAITERS]
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
waiters=${1:-4}
base=${TMPDIR:-.work/tmp}
mkdir -p "$base"
tmp=$(mktemp -d "$base/publish-lock.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
lock=$tmp/publish.lock
log=$tmp/writers.log

mkdir "$lock"
printf '%s\n' 999999 >"$lock/pid"

pids=""
i=0
while [ "$i" -lt "$waiters" ]; do
  i=$((i + 1))
  (
    . "$here/publish-lock.sh"
    acquire_publish_lock "$lock"
    printf 'enter %s\n' "$i" >>"$log"
    sleep 0.2
    printf 'exit %s\n' "$i" >>"$log"
    release_publish_lock
  ) &
  pids="$pids $!"
done
for pid in $pids; do
  wait "$pid"
done

enters=$(awk '$1 == "enter" { n++ } END { print n + 0 }' "$log")
[ "$enters" -eq "$waiters" ] || {
  echo "expected $waiters writers, saw $enters"
  exit 1
}
awk '
  $1 == "enter" {
    if (held != "") {
      printf "overlap: %s entered while %s held the lock\n", $2, held
      bad = 1
    }
    held = $2
  }
  $1 == "exit" {
    if (held != $2) {
      printf "unmatched exit: %s (holder %s)\n", $2, held
      bad = 1
    }
    held = ""
  }
  END {
    if (held != "") {
      printf "missing exit for %s\n", held
      bad = 1
    }
    exit bad
  }
' "$log"

echo "publish lock serialized $waiters writers"
