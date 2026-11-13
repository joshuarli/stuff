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
SERVE_PID=""
trap 'if [ -n "$SERVE_PID" ]; then kill "$SERVE_PID" 2>/dev/null || true; fi; exec 3>&-; wait "$FAKE_PID" 2>/dev/null || true' EXIT
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

# An absent remote object is repaired by the next publish before promotion.
use_home main
BLOB_KEY=$(remote_keys stuff-oci-blobs | grep '^sha256/' | head -1)
[ -n "$BLOB_KEY" ] || fail "no remote blobs to delete"
curl -fsS -X DELETE "$ENDPOINT/stuff-oci-blobs/$BLOB_KEY" >/dev/null || fail "delete of the remote blob failed"
[ "$(curl -s -o /dev/null -w '%{http_code}' "$ENDPOINT/stuff-oci-blobs/$BLOB_KEY")" = "404" ] || fail "the blob was not deleted"
"$STUFF" publish oci --layout "$LAYOUT" --repository example/app >"$WORK/repair.out" || { cat "$WORK/repair.out" >&2; fail "the repair publish failed"; }
[ "$(curl -s -o /dev/null -w '%{http_code}' "$ENDPOINT/stuff-oci-blobs/$BLOB_KEY")" = "200" ] || fail "the missing remote blob was not repaired"
[ "$(live_current stuff-oci)" = "$GEN1" ] || fail "the repair publish moved current"

# A conflicting immutable remote object fails before promotion; the old current stays usable.
printf 'conflicting bytes' >"$WORK/conflict.bin"
curl -fsS -X PUT --data-binary "@$WORK/conflict.bin" "$ENDPOINT/stuff-oci-blobs/$BLOB_KEY" >/dev/null || fail "could not seed the conflicting object"
set +e
"$STUFF" publish oci --layout "$LAYOUT" --repository example/app >"$WORK/conflict.out" 2>&1
conflict_code=$?
set -e
[ "$conflict_code" = "3" ] || { cat "$WORK/conflict.out" >&2; fail "a conflicting remote object exited $conflict_code, expected 3"; }
grep -q "content-addressed blobs" "$WORK/conflict.out" || { cat "$WORK/conflict.out" >&2; fail "the conflicting publication gave no immutable-conflict reason"; }
[ "$(live_current stuff-oci)" = "$GEN1" ] || fail "the conflicting publication moved current"
python3 "$ROOT/deploy/r2-pull.py" stuff-oci "$WORK/pull-after-conflict" >/dev/null || fail "the previously promoted generation is not usable after a failed publication"
curl -fsS -X DELETE "$ENDPOINT/stuff-oci-blobs/$BLOB_KEY" >/dev/null || fail "could not clear the conflicting object"
"$STUFF" publish oci --layout "$LAYOUT" --repository example/app >"$WORK/conflict-repair.out" || { cat "$WORK/conflict-repair.out" >&2; fail "the re-publication after clearing the conflict failed"; }
[ "$(curl -s -o /dev/null -w '%{http_code}' "$ENDPOINT/stuff-oci-blobs/$BLOB_KEY")" = "200" ] || fail "the cleared blob was not re-uploaded"
[ "$(live_current stuff-oci)" = "$GEN1" ] || fail "the re-publication after clearing the conflict moved current"

# Mirror input mode end to end against a loopback registry.
MIRROR_STATE=$WORK/mirror-serve-state
"$MYOCI" init --state-dir "$MIRROR_STATE" >/dev/null
"$MYOCI" import --state-dir "$MIRROR_STATE" --layout "$LAYOUT" --repository example/app --tag stable >/dev/null || fail "fixture import for the mirror source failed"
MIRROR_SITE=$WORK/mirror-serve-site
"$MYOCI" publish --state-dir "$MIRROR_STATE" --site-dir "$MIRROR_SITE" >/dev/null || fail "fixture publish for the mirror source failed"
"$MYOCI" serve --site-dir "$MIRROR_SITE" --bind 127.0.0.1:0 >"$WORK/mirror-serve.out" 2>&1 &
SERVE_PID=$!
for _ in $(seq 50); do
  grep -q '^listening on ' "$WORK/mirror-serve.out" 2>/dev/null && break
  sleep 0.1
done
REG_ADDR=$(sed -n 's/^listening on //p' "$WORK/mirror-serve.out" | tr -d '[:space:]')
[ -n "$REG_ADDR" ] || fail "the loopback registry did not start"
cat >"$WORK/mirror.toml" <<EOF
schema_version = 1
policy_version = 1

[[entries]]
id = "sim"
source = "$REG_ADDR/example/app:stable"
destination = "docker.io/example/mirror-sim"
aliases = ["pinned"]
platforms = ["linux/amd64"]
referrers = "none"
EOF
"$MYOCI" mirror resolve --config "$WORK/mirror.toml" --lock "$WORK/mirror.lock" >/dev/null || fail "mirror resolve failed"
use_home mirror
mkdir -p "$STUFF_CONFIG_HOME"
printf '%s\n' '{"oci": {"bucket": "stuff-oci", "blobs_bucket": "stuff-oci-blobs"}}' >"$STUFF_CONFIG_HOME/config.json"
"$STUFF" publish oci --inventory "$WORK/mirror.toml" --lock "$WORK/mirror.lock" >"$WORK/mirror.out" || { cat "$WORK/mirror.out" >&2; fail "the mirror publication failed"; }
MIRROR_GEN=$(local_current mirror)
[ "$(live_current stuff-oci)" = "$MIRROR_GEN" ] || fail "the mirror publication did not promote $MIRROR_GEN"
remote_keys stuff-oci >"$WORK/mirror.keys"
grep -q "^generations/$MIRROR_GEN/provenance.json$" "$WORK/mirror.keys" || fail "the published generation carries no provenance sidecar remotely"
curl -fsS "$ENDPOINT/stuff-oci/generations/$MIRROR_GEN/provenance.json" | grep -q '"id":"sim"' || fail "the remote provenance sidecar is not the mirror's"
grep -q '"pinned"' "$STUFF_STATE_HOME/oci/state/repositories/docker.io/example/mirror-sim/repository.json" || fail "the managed alias did not land in the mirrored repository"
"$MYOCI" verify --state-dir "$STUFF_STATE_HOME/oci/state" >/dev/null || fail "the mirror-published state does not verify"
"$STUFF" publish oci --inventory "$WORK/mirror.toml" --lock "$WORK/mirror.lock" >"$WORK/mirror-replay.out" || fail "the mirror replay failed"
grep -q unchanged "$WORK/mirror-replay.out" || fail "the mirror replay was not a no-op: $(cat "$WORK/mirror-replay.out")"

# The --image input mode works end to end against the loopback registry.
use_home image
mkdir -p "$STUFF_CONFIG_HOME"
printf '%s\n' '{"oci": {"bucket": "image-oci", "blobs_bucket": "image-oci-blobs"}}' >"$STUFF_CONFIG_HOME/config.json"
"$STUFF" publish oci --image "example/app=$REG_ADDR/example/app:stable" >"$WORK/image.out" || { cat "$WORK/image.out" >&2; fail "the --image publication failed"; }
IMAGE_GEN=$(local_current image)
[ "$(live_current image-oci)" = "$IMAGE_GEN" ] || fail "the --image publication did not promote $IMAGE_GEN"
kill "$SERVE_PID" 2>/dev/null || true
SERVE_PID=""

# A home pinned to an older live generation is refused at promotion; the
# competing generation survives and the previously promoted generation is usable.
use_home stale
mkdir -p "$STUFF_CONFIG_HOME"
printf '%s\n' '{"oci": {"bucket": "stale-oci", "blobs_bucket": "stale-oci-blobs"}}' >"$STUFF_CONFIG_HOME/config.json"
"$STUFF" publish oci --layout "$LAYOUT" --repository example/app >/dev/null || fail "the stale-home first publish failed"
GEN_A=$(local_current stale)
use_home stale2
mkdir -p "$STUFF_CONFIG_HOME"
printf '%s\n' '{"oci": {"bucket": "stale-oci", "blobs_bucket": "stale-oci-blobs"}}' >"$STUFF_CONFIG_HOME/config.json"
"$STUFF" publish oci --layout "$LAYOUT" --repository example/competing >/dev/null || fail "the competing publication failed"
GEN_B=$(local_current stale2)
[ "$GEN_B" != "$GEN_A" ] || fail "the competing publication did not make a new generation"
[ "$(live_current stale-oci)" = "$GEN_B" ] || fail "the competing publication did not promote"
use_home stale
[ "$(local_current stale)" = "$GEN_A" ] || fail "the competing publication changed this home's local base"
set +e
"$STUFF" publish oci --layout "$LAYOUT" --repository example/app --tag bump >"$WORK/stale.out" 2>&1
stale_code=$?
set -e
[ "$stale_code" = "3" ] || { cat "$WORK/stale.out" >&2; fail "the stale promotion was not refused (exit $stale_code)"; }
grep -q "refusing to overwrite a competing publication" "$WORK/stale.out" || { cat "$WORK/stale.out" >&2; fail "the stale promotion was refused without a reason"; }
[ "$(live_current stale-oci)" = "$GEN_B" ] || fail "the stale publication overwrote the competing generation"
[ "$(local_current stale)" != "$(live_current stale-oci)" ] || fail "the refused promotion left local and live indistinguishable"
python3 "$ROOT/deploy/r2-pull.py" stale-oci "$WORK/pull-stale-survivor" >/dev/null || fail "the competing generation is not pullable after the refused promotion"

# The OCI flow must not mutate any non-OCI bucket.
curl -fsS "$ENDPOINT/_fake/requests" | python3 -c '
import json, sys
requests = json.load(sys.stdin)["requests"]
allowed = ("/stuff-oci", "/image-oci", "/stale-oci", "/_fake")
bad = [entry for entry in requests if entry["method"] in ("PUT", "POST", "DELETE") and not entry["path"].startswith(allowed)]
if bad:
    for entry in bad:
        print("{0} {1}".format(entry["method"], entry["path"]), file=sys.stderr)
    sys.exit(1)
print("{0} fake-S3 requests audited: no non-OCI bucket mutations".format(len(requests)))
' || fail "an OCI publication mutated a non-OCI bucket"

echo "OK: OCI publish, blob sync, blob GC, rollback, adoption, repair, conflict, mirror, image, stale promotion, and bucket isolation are green ($GEN1 -> $GEN2)"
