#!/bin/sh
# Push a two-platform image through the OCI front and writer, pull it back,
# attach a referrer, mount across repositories, and check the error cases.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-"$ROOT/.work/oci-push"}
TMPDIR=${TMPDIR:-"$ROOT/.work/tmp"}
export TMPDIR
mkdir -p "$TMPDIR"
rm -rf "$WORK"
mkdir -p "$WORK"
LAYER_BYTES=${PUSH_LAYER_BYTES:-1073741824}

fail() {
  echo "FAIL: $1" >&2
  [ -f "$WORK/front.log" ] && sed -n '1,40p' "$WORK/front.log" >&2
  [ -f "$WORK/writer.log" ] && sed -n '1,40p' "$WORK/writer.log" >&2
  [ -f "$WORK/build.log" ] && tail -40 "$WORK/build.log" >&2
  exit 1
}
command -v docker >/dev/null || fail "docker is required"
command -v oras >/dev/null 2>&1 || {
  GOBIN=${GOBIN:-"$(go env GOPATH)/bin"}
  [ -x "$GOBIN/oras" ] || (cd "$ROOT" && go install oras.land/oras/cmd/oras@v1.3.6)
  PATH="$GOBIN:$PATH"; export PATH
}
command -v jaq >/dev/null || fail "jaq is required"

(cd "$ROOT" && cargo build -q -p myoci)
BIN=${CARGO_TARGET_DIR:-"$ROOT/target"}/debug/myoci
STATE=$WORK/state
SITE=$WORK/site
TOKEN=$WORK/token
SECRET=$WORK/secret
printf 'push-token' > "$TOKEN"
printf 'push-secret' > "$SECRET"
"$BIN" init --state-dir "$STATE" >/dev/null
"$BIN" publish --state-dir "$STATE" --site-dir "$SITE" >/dev/null
PP=$(python3 - <<'PY'
import socket
sock = socket.socket()
sock.bind(("127.0.0.1", 0))
print(sock.getsockname()[1])
PY
)

"$BIN" writer --state-dir "$STATE" --bind 127.0.0.1:0 --secret-file "$SECRET" \
  --public-origin "http://127.0.0.1:$PP" > "$WORK/writer.log" 2>&1 &
WRITER_PID=$!
WP=""
tries=0
while [ -z "$WP" ] && [ "$tries" -lt 100 ]; do
  WP=$(sed -n 's/^listening on 127\.0\.0\.1://p' "$WORK/writer.log" 2>/dev/null)
  tries=$((tries + 1))
  [ -n "$WP" ] || sleep 0.1
done
[ -n "$WP" ] || fail "writer did not start"
"$BIN" serve --site-dir "$SITE" --state-dir "$STATE" --push \
  --writer-url "http://127.0.0.1:$PP" --push-token-file "$TOKEN" \
  --push-secret-file "$SECRET" --bind 127.0.0.1:0 > "$WORK/front.log" 2>&1 &
FRONT_PID=$!
FP=""
tries=0
while [ -z "$FP" ] && [ "$tries" -lt 100 ]; do
  FP=$(sed -n 's/^listening on 127\.0\.0\.1://p' "$WORK/front.log" 2>/dev/null)
  tries=$((tries + 1))
  [ -n "$FP" ] || sleep 0.1
done
[ -n "$FP" ] || fail "front did not start"
python3 - "$PP" "$FP" "$WP" <<'PY' > "$WORK/proxy.log" 2>&1 &
import socket
import sys
import threading

public, front, writer = (int(value) for value in sys.argv[1:4])


def pipe(source, destination):
    try:
        while True:
            data = source.recv(1 << 16)
            if not data:
                break
            destination.sendall(data)
    except OSError:
        pass
    finally:
        try:
            destination.shutdown(socket.SHUT_WR)
        except OSError:
            pass


def handle(client):
    data = b""
    while b"\r\n\r\n" not in data and len(data) < 65536:
        chunk = client.recv(65536)
        if not chunk:
            client.close()
            return
        data += chunk
    line = data.split(b"\r\n", 1)[0]
    parts = line.split(b" ")
    path = parts[1] if len(parts) > 1 else b""
    route = path.split(b"?", 1)[0]
    upload = b"/blobs/uploads/" in route and route.split(b"/blobs/uploads/", 1)[1] != b""
    backend = writer if upload else front
    try:
        remote = socket.create_connection(("127.0.0.1", backend))
    except OSError:
        client.close()
        return
    remote.sendall(data)
    relay = threading.Thread(target=pipe, args=(remote, client), daemon=True)
    relay.start()
    pipe(client, remote)
    relay.join()
    client.close()
    remote.close()


listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", public))
listener.listen(128)
print(f"listening on 127.0.0.1:{public}", flush=True)
while True:
    connection, _ = listener.accept()
    threading.Thread(target=handle, args=(connection,), daemon=True).start()
PY
PROXY_PID=$!
sleep 0.2
REGISTRY=127.0.0.1:$PP
FRONT=http://$REGISTRY
REF=$REGISTRY/example/push:stable
IMAGE=$REGISTRY/example/push
cleanup() {
  status=$?
  trap - EXIT INT TERM
  kill "$FRONT_PID" "$WRITER_PID" "$PROXY_PID" >/dev/null 2>&1 || true
  wait "$FRONT_PID" "$WRITER_PID" "$PROXY_PID" 2>/dev/null || true
  docker image rm -f "$REF" >/dev/null 2>&1 || true
  docker image rm -f "$IMAGE@$DIGEST" >/dev/null 2>&1 || true
  docker logout "$REGISTRY" >/dev/null 2>&1 || true
  if [ "$status" -eq 0 ] && [ -z "${KEEP:-}" ]; then
    rm -rf "$WORK"
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

printf '%s' "$(cat "$TOKEN")" | docker login "$REGISTRY" -u stuff --password-stdin >/dev/null || fail "docker login failed"

mkdir -p "$WORK/context"
python3 - "$WORK/context/big.bin" "$LAYER_BYTES" <<'PY'
import sys
path, size = sys.argv[1], int(sys.argv[2])
chunk = bytes(range(256)) * 4096
with open(path, "wb") as f:
    remaining = size
    while remaining:
        piece = chunk[: min(len(chunk), remaining)]
        f.write(piece)
        remaining -= len(piece)
PY
printf 'FROM scratch\nCOPY big.bin /big.bin\n' > "$WORK/context/Dockerfile"

docker buildx build --push --platform linux/arm64,linux/amd64 \
  --provenance=false --sbom=false -t "$REF" "$WORK/context" > "$WORK/build.log" 2>&1 \
  || fail "docker buildx push failed"
DIGEST=$(docker buildx imagetools inspect --raw "$REF" | python3 -c 'import hashlib,sys; print("sha256:" + hashlib.sha256(sys.stdin.buffer.read()).hexdigest())')
[ -n "$DIGEST" ] || fail "could not read the pushed index digest"

docker pull -q "$IMAGE@$DIGEST" >/dev/null || fail "docker pull by digest failed"
GEN1=$(readlink "$SITE/current")
docker buildx build --push --platform linux/arm64,linux/amd64 \
  --provenance=false --sbom=false -t "$REF" "$WORK/context" > "$WORK/rebuild.log" 2>&1 \
  || fail "docker re-push failed"
GEN2=$(readlink "$SITE/current")
[ "$GEN1" = "$GEN2" ] || fail "re-push changed the generation"

printf '{"spdxVersion":"SPDX-2.3"}\n' > "$WORK/sbom.json"
oras login --plain-http "$REGISTRY" -u stuff -p "$(cat "$TOKEN")" >/dev/null || fail "oras login failed"
if ! oras attach --plain-http --disable-path-validation --artifact-type application/vnd.test.sbom "$REF" "$WORK/sbom.json" > "$WORK/attach.log" 2>&1; then
  cat "$WORK/attach.log" >&2
  fail "oras attach failed"
fi
DISCOVER=$(oras discover --plain-http --format json "$REF")
printf '%s' "$DISCOVER" | jaq -e '(.referrers | length) > 0' >/dev/null || fail "oras discover found no referrer"

INDEX_JSON=$(oras manifest fetch --plain-http "$REF")
CHILD=$(printf '%s' "$INDEX_JSON" | jaq -r '.manifests[0].digest')
CHILD_JSON=$(oras manifest fetch --plain-http "$IMAGE@$CHILD")
LAYER=$(printf '%s' "$CHILD_JSON" | jaq -r '.layers[0].digest')
MOUNT=$(curl -sS -u stuff:$(cat "$TOKEN") -D - -o /dev/null \
  -X POST "$FRONT/v2/example/other/blobs/uploads/?mount=$LAYER&from=example/push")
printf '%s' "$MOUNT" | grep -qi '^HTTP/1.1 201' || fail "cross-repository mount did not hit"

CODE=$(curl -sS -o "$WORK/body" -D "$WORK/headers" -w '%{http_code}' \
  -X POST "$FRONT/v2/example/auth/blobs/uploads/")
[ "$CODE" = 401 ] || fail "unauthenticated upload returned $CODE"
grep -qi '^www-authenticate: Basic realm="stuff-oci"' "$WORK/headers" || fail "missing auth challenge"

MISSING=$(python3 - <<'PY'
import json
print(json.dumps({
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": "sha256:" + "1" * 64, "size": 2},
    "layers": [],
}))
PY
)
CODE=$(printf '%s' "$MISSING" | curl -sS -u stuff:$(cat "$TOKEN") -o "$WORK/body" -w '%{http_code}' \
  -X PUT --data-binary @- "$FRONT/v2/example/push/manifests/missing")
[ "$CODE" = 400 ] || fail "missing blob manifest returned $CODE"
grep -q MANIFEST_BLOB_UNKNOWN "$WORK/body" || fail "missing blob code"

python3 - "$STATE/.push.lock" <<'PY' &
import fcntl
import sys
import time
lock = open(sys.argv[1], "a+")
fcntl.flock(lock, fcntl.LOCK_EX)
time.sleep(6)
PY
LOCK_PID=$!
sleep 0.3
CODE=$(printf '%s' "$INDEX_JSON" | curl -sS -u stuff:$(cat "$TOKEN") -o "$WORK/body" -w '%{http_code}' \
  -X PUT --data-binary @- "$FRONT/v2/example/push/manifests/stable")
kill "$LOCK_PID" >/dev/null 2>&1 || true
wait "$LOCK_PID" 2>/dev/null || true
[ "$CODE" = 409 ] || fail "locked push returned $CODE, expected 409"

echo "OK: docker push/pull, oras referrer, mount, 401/400/409, and a no-op re-push"
