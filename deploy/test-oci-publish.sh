#!/bin/sh
# Publish the OCI fixture under the local R2 simulation, sync blobs before
# metadata, reclaim an unreferenced blob, and roll back a tag move.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-"$ROOT/.work/oci-publish"}
TMPDIR=${TMPDIR:-"$ROOT/.work/tmp"}
export TMPDIR
mkdir -p "$TMPDIR"
rm -rf "$WORK"
mkdir -p "$WORK"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}
if ! command -v cf >/dev/null 2>&1; then
  CF_BIN=$ROOT/crates/stuff-worker/node_modules/.bin
  [ -x "$CF_BIN/cf" ] || fail "the cf CLI is required"
  PATH="$CF_BIN:$PATH"
  export PATH
fi
export STUFF_STATE_HOME="$WORK/home"
export CF_LOCAL_STATE="$WORK/r2"
export CF_AUTH_CHECKED=1

(cd "$ROOT" && cargo build -q -p myoci)
BIN=${CARGO_TARGET_DIR:-"$ROOT/target"}/debug/myoci
STATE=$STUFF_STATE_HOME/oci/state
SITE=$STUFF_STATE_HOME/oci/site

list_bucket() {
  cf r2 objects list --per-page 1000 --bucket-name "$1" \
    --local --persist-to "$CF_LOCAL_STATE" > "$WORK/$1.list.json"
}
current_gen() {
  cf r2 objects get current --text --bucket-name "$1" \
    --local --persist-to "$CF_LOCAL_STATE"
}

"$BIN" init --state-dir "$STATE" >/dev/null
"$BIN" import --state-dir "$STATE" --layout "$ROOT/tests/fixtures/oci/layout" --repository example/app >/dev/null
"$BIN" publish --state-dir "$STATE" --site-dir "$SITE" >/dev/null
GEN1=$(basename "$(readlink "$SITE/current")")

(cd "$ROOT" && deploy/publish oci --republish --promote=cf >/dev/null)
list_bucket stuff-oci
list_bucket stuff-oci-blobs
python3 - "$WORK/stuff-oci.list.json" "$WORK/stuff-oci-blobs.list.json" "$GEN1" <<'PY'
import json
import sys
meta = [entry["key"] for entry in json.load(open(sys.argv[1], encoding="utf-8"))]
blobs = [entry["key"] for entry in json.load(open(sys.argv[2], encoding="utf-8"))]
generation = sys.argv[3]
assert "current" in meta, meta
assert f"generations/{generation}/catalog.json" in meta, meta
assert f"manifests/{generation}.json" in meta, meta
assert sum(key.startswith("oci/manifests/sha256/") for key in meta) == 4, meta
assert not any(key.startswith("blobs/") for key in meta), meta
assert len([key for key in blobs if key.startswith("sha256/")]) == 6, blobs
PY

ORPHAN_FILE=$WORK/orphan
echo orphan > "$ORPHAN_FILE"
if command -v sha256sum >/dev/null 2>&1; then
  ORPHAN=$(sha256sum "$ORPHAN_FILE" | cut -d' ' -f1)
else
  ORPHAN=$(shasum -a 256 "$ORPHAN_FILE" | cut -d' ' -f1)
fi
cf r2 objects put "sha256/$ORPHAN" --file "$ORPHAN_FILE" \
  --bucket-name stuff-oci-blobs --local --persist-to "$CF_LOCAL_STATE" >/dev/null

python3 "$ROOT/deploy/r2-gc-blobs.py" stuff-oci stuff-oci-blobs --dry-run > "$WORK/gc-dry.txt"
grep -q "drop sha256/$ORPHAN" "$WORK/gc-dry.txt" || {
  cat "$WORK/gc-dry.txt" >&2
  fail "dry run did not plan the unreferenced blob"
}
python3 "$ROOT/deploy/r2-gc-blobs.py" stuff-oci stuff-oci-blobs > "$WORK/gc.txt"
list_bucket stuff-oci-blobs
python3 - "$WORK/stuff-oci-blobs.list.json" "$ORPHAN" <<'PY'
import json
import sys
keys = [entry["key"] for entry in json.load(open(sys.argv[1], encoding="utf-8"))]
orphan = sys.argv[2]
assert f"sha256/{orphan}" not in keys, keys
assert len([key for key in keys if key.startswith("sha256/")]) == 6, keys
PY

"$BIN" tag --state-dir "$STATE" --repository example/app --tag stable \
  --digest sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4 >/dev/null
(cd "$ROOT" && deploy/publish oci --republish --promote=cf >/dev/null)
GEN2=$(basename "$(readlink "$SITE/current")")
[ "$GEN2" != "$GEN1" ] || fail "tag move did not create a new generation"
(cd "$ROOT" && deploy/r2-rollback.sh stuff-oci "$GEN1" --promote=cf >/dev/null)
[ "$(current_gen stuff-oci)" = "$GEN1" ] || fail "rollback did not restore the first generation"

STUFF_STATE_HOME="$WORK/home-adopt" TMPDIR="$TMPDIR" deploy/publish oci --republish --promote=cf >/dev/null
"$BIN" verify --state-dir "$WORK/home-adopt/oci/state" >/dev/null
ADOPTED=$(basename "$(readlink "$WORK/home-adopt/oci/site/current")")
[ "$ADOPTED" = "$GEN1" ] || fail "adoption restored $ADOPTED, expected $GEN1"

echo "OK: OCI publish, blob sync, blob GC, rollback, and adoption are green ($GEN1 -> $GEN2)"
