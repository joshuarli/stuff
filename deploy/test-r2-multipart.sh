#!/bin/sh
# Send a 1 GiB blob through the local front to the writer's R2 sink and
# verify the object in R2. Requires the S3 API credentials.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-"$ROOT/.work/oci-r2-multipart"}
TMPDIR=${TMPDIR:-"$ROOT/.work/tmp"}
export TMPDIR
for var in R2_S3_ENDPOINT R2_S3_ACCESS_KEY_ID R2_S3_SECRET_ACCESS_KEY; do
  eval "value=\${$var:-}"
  [ -n "$value" ] || { echo "FAIL: $var is required for the live writer multipart test" >&2; exit 2; }
done
BUCKET=${BUCKET_BLOBS:-stuff-oci-blobs}
rm -rf "$WORK"
mkdir -p "$WORK"
(cd "$ROOT" && cargo build -q -p myoci)
BIN=${CARGO_TARGET_DIR:-"$ROOT/target"}/debug/myoci
STATE=$WORK/state
SITE=$WORK/site
printf 'secret' > "$WORK/secret"
printf 'token' > "$WORK/token"
"$BIN" init --state-dir "$STATE" >/dev/null
"$BIN" publish --state-dir "$STATE" --site-dir "$SITE" >/dev/null
"$BIN" writer --state-dir "$STATE" --bind 127.0.0.1:0 --secret-file "$WORK/secret" \
  --r2-bucket "$BUCKET" --deploy-dir "$ROOT/deploy" > "$WORK/writer.log" 2>&1 &
WRITER_PID=$!
WP=""
tries=0
while [ -z "$WP" ] && [ "$tries" -lt 100 ]; do
  WP=$(sed -n 's/^listening on 127\.0\.0\.1://p' "$WORK/writer.log" 2>/dev/null)
  tries=$((tries + 1))
  [ -n "$WP" ] || sleep 0.1
done
[ -n "$WP" ] || { cat "$WORK/writer.log" >&2; exit 1; }
"$BIN" serve --site-dir "$SITE" --state-dir "$STATE" --push \
  --writer-url "http://127.0.0.1:$WP" --push-token-file "$WORK/token" \
  --push-secret-file "$WORK/secret" --bind 127.0.0.1:0 > "$WORK/front.log" 2>&1 &
FRONT_PID=$!
cleanup() {
  status=$?
  trap - EXIT INT TERM
  kill "$FRONT_PID" "$WRITER_PID" >/dev/null 2>&1 || true
  wait "$FRONT_PID" "$WRITER_PID" 2>/dev/null || true
  exit "$status"
}
trap cleanup EXIT INT TERM
FP=""
tries=0
while [ -z "$FP" ] && [ "$tries" -lt 100 ]; do
  FP=$(sed -n 's/^listening on 127\.0\.0\.1://p' "$WORK/front.log" 2>/dev/null)
  tries=$((tries + 1))
  [ -n "$FP" ] || sleep 0.1
done
[ -n "$FP" ] || exit 1
python3 - "$WORK/blob" <<'PY'
import sys
chunk = bytes(range(256)) * 4096
with open(sys.argv[1], "wb") as f:
    for _ in range(1024):
        f.write(chunk)
PY
# Hash in one streaming pass without loading the GiB into memory.
HEX=$(python3 - "$WORK/blob" <<'PY'
import hashlib
import sys
digest = hashlib.sha256()
with open(sys.argv[1], "rb") as f:
    for chunk in iter(lambda: f.read(1 << 20), b""):
        digest.update(chunk)
print(digest.hexdigest())
PY
)
LOCATION=$(curl -sS -u stuff:$(cat "$WORK/token") -X POST -D - -o /dev/null \
  "http://127.0.0.1:$FP/v2/example/r2/blobs/uploads/" | sed -n 's/^location: //Ip' | tr -d '\r')
[ -n "$LOCATION" ] || { echo "FAIL: no writer location" >&2; exit 1; }
curl -sS -f -X PATCH --data-binary @"$WORK/blob" "$LOCATION" >/dev/null
curl -sS -f -X PUT --data-binary '' "$LOCATION&digest=sha256:$HEX" >/dev/null
HEAD=$(cf r2 objects head "sha256/$HEX" --bucket-name "$BUCKET" 2>/dev/null || true)
[ -n "$HEAD" ] || { echo "FAIL: R2 object sha256/$HEX is missing" >&2; exit 1; }
SIZE=$(printf '%s' "$HEAD" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("size"))')
[ "$SIZE" = "1073741824" ] || { echo "FAIL: R2 object size is $SIZE" >&2; exit 1; }
echo "OK: 1 GiB blob uploaded through the writer as sha256/$HEX"
