#!/bin/sh
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-"$ROOT/.work/oci-publish"}
TMPDIR=${TMPDIR:-"$ROOT/.work/tmp"}
export TMPDIR
mkdir -p "$TMPDIR"
rm -rf "$WORK"
mkdir -p "$WORK"
LAYOUT=$ROOT/tests/fixtures/oci/layout

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

(cd "$ROOT" && cargo build -q -p stuff -p myoci)
BIN_DIR=${CARGO_TARGET_DIR:-"$ROOT/target"}/debug
STUFF=$BIN_DIR/stuff
export STUFF
MYOCI=$BIN_DIR/myoci

FIFO=$WORK/fake-s3.stdin
mkfifo "$FIFO"
python3 "$ROOT/deploy/fake_s3.py" <"$FIFO" >"$WORK/fake-s3.out" &
FAKE_PID=$!
exec 3>"$FIFO"
trap 'exec 3>&-; wait "$FAKE_PID" 2>/dev/null || true' EXIT
for _ in $(seq 50); do
  grep -q '^PORT ' "$WORK/fake-s3.out" 2>/dev/null && break
  sleep 0.1
done
PORT=$(sed -n 's/^PORT //p' "$WORK/fake-s3.out")
[ -n "$PORT" ] || fail "fake S3 did not start"
ENDPOINT=http://127.0.0.1:$PORT
export R2_S3_ENDPOINT="$ENDPOINT" R2_S3_ACCESS_KEY_ID=test R2_S3_SECRET_ACCESS_KEY=secret

use_home() {
  export STUFF_STATE_HOME="$WORK/$1/state" STUFF_CACHE_HOME="$WORK/$1/cache" STUFF_CONFIG_HOME="$WORK/$1/config"
}
live_current() {
  curl -fsS "$ENDPOINT/$1/current" | tr -d '[:space:]'
}
local_current() {
  basename "$(readlink "$WORK/$1/state/oci/site/current")"
}
remote_keys() {
  curl -fsS "$ENDPOINT/_fake/state" | python3 -c '
import json, sys
bucket = sys.argv[1]
for item in json.load(sys.stdin)["objects"]:
    if item["bucket"] == bucket:
        print(item["key"])
' "$1"
}

use_home empty
mkdir -p "$STUFF_CONFIG_HOME"
printf '%s\n' '{"oci": {"bucket": "empty-oci", "blobs_bucket": "empty-oci-blobs"}}' >"$STUFF_CONFIG_HOME/config.json"
if "$STUFF" republish oci >/dev/null 2>"$WORK/empty-republish.err"; then
  fail "republish of empty buckets succeeded"
fi
grep -q "empty-oci has no live publication to republish" "$WORK/empty-republish.err" || {
  cat "$WORK/empty-republish.err" >&2
  fail "republish of empty buckets gave no clear reason"
}

use_home main
"$STUFF" publish oci --layout "$LAYOUT" --repository example/app >/dev/null || fail "first publication into empty buckets failed"
GEN1=$(local_current main)
[ "$(live_current stuff-oci)" = "$GEN1" ] || fail "first publication promoted '$(live_current stuff-oci)', expected '$GEN1'"
remote_keys stuff-oci >"$WORK/meta.keys"
remote_keys stuff-oci-blobs >"$WORK/blobs.keys"
python3 - "$WORK/meta.keys" "$WORK/blobs.keys" "$GEN1" <<'PY'
import sys
meta = open(sys.argv[1], encoding="utf-8").read().split()
blobs = open(sys.argv[2], encoding="utf-8").read().split()
generation = sys.argv[3]
assert "current" in meta, meta
assert f"generations/{generation}/catalog.json" in meta, meta
assert f"manifests/{generation}.json" in meta, meta
assert sum(key.startswith("oci/manifests/sha256/") for key in meta) == 4, meta
assert not any(key.startswith("blobs/") for key in meta), meta
assert len([key for key in blobs if key.startswith("sha256/")]) == 6, blobs
PY

ORPHAN_FILE=$WORK/orphan
echo orphan >"$ORPHAN_FILE"
ORPHAN=$(python3 -c 'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$ORPHAN_FILE")
curl -fsS -X PUT --data-binary "@$ORPHAN_FILE" "$ENDPOINT/stuff-oci-blobs/sha256/$ORPHAN" >/dev/null
python3 "$ROOT/deploy/r2-gc-blobs.py" stuff-oci stuff-oci-blobs --grace-minutes 0 --dry-run >"$WORK/gc-dry.txt"
grep -q "drop sha256/$ORPHAN" "$WORK/gc-dry.txt" || {
  cat "$WORK/gc-dry.txt" >&2
  fail "dry run did not plan the unreferenced blob"
}
python3 "$ROOT/deploy/r2-gc-blobs.py" stuff-oci stuff-oci-blobs --grace-minutes 0 >"$WORK/gc.txt"
remote_keys stuff-oci-blobs >"$WORK/blobs-after.keys"
! grep -qx "sha256/$ORPHAN" "$WORK/blobs-after.keys" || fail "gc kept the unreferenced blob"
[ "$(grep -c '^sha256/' "$WORK/blobs-after.keys")" = 6 ] || fail "gc dropped a referenced blob"

"$STUFF" publish oci --layout "$LAYOUT" --repository example/app --tag moved >/dev/null
GEN2=$(local_current main)
[ "$GEN2" != "$GEN1" ] || fail "tag move did not create a new generation"
[ "$(live_current stuff-oci)" = "$GEN2" ] || fail "tag move did not promote $GEN2"
"$STUFF" rollback oci "$GEN1" >/dev/null
[ "$(live_current stuff-oci)" = "$GEN1" ] || fail "rollback did not restore the first generation"

use_home adopt
"$STUFF" publish oci --layout "$LAYOUT" --repository example/app >"$WORK/adopt.out"
grep -q unchanged "$WORK/adopt.out" || fail "a fresh state home republished the live generation: $(cat "$WORK/adopt.out")"
[ "$(live_current stuff-oci)" = "$GEN1" ] || fail "adoption moved current"
"$MYOCI" verify --state-dir "$WORK/adopt/state/oci/state" >/dev/null || fail "adopted state does not verify"

echo "OK: OCI publish, blob sync, blob GC, rollback, and adoption are green ($GEN1 -> $GEN2)"
