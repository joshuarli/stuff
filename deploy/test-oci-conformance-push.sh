#!/bin/sh
# Run the pinned push conformance suite against a local front and writer.
# The suite generates its own image data; the registry starts empty.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TMPDIR=${TMPDIR:-$ROOT/.work/tmp}
export TMPDIR
mkdir -p "$TMPDIR"
WORK=$(mktemp -d "$TMPDIR/oci-conformance-push.XXXXXX")
FRONT_PID=
WRITER_PID=
cleanup() {
  status=$?
  trap - EXIT
  [ -z "$FRONT_PID" ] || kill "$FRONT_PID" 2>/dev/null || true
  [ -z "$WRITER_PID" ] || kill "$WRITER_PID" 2>/dev/null || true
  wait 2>/dev/null || true
  exit "$status"
}
trap cleanup EXIT INT TERM

cargo build -q --manifest-path "$ROOT/Cargo.toml" -p myoci
BIN=$ROOT/target/debug/myoci
"$BIN" init --state-dir "$WORK/state" >/dev/null
"$BIN" publish --state-dir "$WORK/state" --site-dir "$WORK/site" >/dev/null
printf 'token' > "$WORK/token"
printf 'secret' > "$WORK/secret"

free_port() {
  python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}
wait_for_log() {
  file=$1
  prefix=$2
  tries=0
  while [ "$tries" -lt 100 ]; do
    value=$(sed -n "s/^$prefix//p" "$file" 2>/dev/null | head -1)
    [ -z "$value" ] || { printf '%s\n' "$value"; return 0; }
    tries=$((tries + 1))
    sleep 0.1
  done
  echo "test-oci-conformance-push: server did not start" >&2
  cat "$file" >&2 || true
  return 1
}

FRONT_PORT=$(free_port)
"$BIN" writer --state-dir "$WORK/state" --bind 127.0.0.1:0 \
  --secret-file "$WORK/secret" --public-origin "http://127.0.0.1:$FRONT_PORT" \
  > "$WORK/writer.log" 2>&1 &
WRITER_PID=$!
WRITER_PORT=$(wait_for_log "$WORK/writer.log" 'listening on 127\.0\.0\.1:')

"$BIN" serve --site-dir "$WORK/site" --state-dir "$WORK/state" --push \
  --writer-url "http://127.0.0.1:$WRITER_PORT" \
  --push-token-file "$WORK/token" --push-secret-file "$WORK/secret" \
  --bind "127.0.0.1:$FRONT_PORT" > "$WORK/front.log" 2>&1 &
FRONT_PID=$!
wait_for_log "$WORK/front.log" 'listening on 127\.0\.0\.1:' >/dev/null

OCI_USERNAME=stuff OCI_PASSWORD=token \
  "$ROOT/deploy/oci-conformance.sh" --push --registry "127.0.0.1:$FRONT_PORT" \
  --results "$WORK/results"
