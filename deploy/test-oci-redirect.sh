#!/bin/sh
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-"$ROOT/.work/oci-redirect"}
TMPDIR=${TMPDIR:-"$ROOT/.work/tmp"}
export TMPDIR
mkdir -p "$TMPDIR"
rm -rf "$WORK"
mkdir -p "$WORK"

fail() {
  echo "FAIL: $1" >&2
  [ -f "$WORK/front.log" ] && sed -n '1,40p' "$WORK/front.log" >&2
  [ -f "$WORK/blob.log" ] && sed -n '1,40p' "$WORK/blob.log" >&2
  exit 1
}
command -v docker >/dev/null || fail "docker is required"
[ -d "$ROOT/crates/stuff-worker/node_modules" ] || fail "run npm ci in crates/stuff-worker"

(cd "$ROOT" && cargo build -q -p myoci && make worker-bundle >/dev/null)
BIN=${CARGO_TARGET_DIR:-"$ROOT/target"}/debug/myoci
SITE=$WORK/site
STATE=$WORK/state
REPO=example/redirect
LAYER=$(python3 - "$WORK/layout" "$(uname -m)" <<'PY'
import gzip
import hashlib
import io
import json
import os
import sys
import tarfile
import time

root, machine = sys.argv[1:3]
arch = {"arm64": "arm64", "aarch64": "arm64", "x86_64": "amd64", "amd64": "amd64"}[machine]
blobs = os.path.join(root, "blobs", "sha256")
os.makedirs(blobs, exist_ok=True)


def put(data):
    digest = hashlib.sha256(data).hexdigest()
    with open(os.path.join(blobs, digest), "wb") as f:
        f.write(data)
    return digest, len(data)


payload = f"redirect {time.time_ns()}".encode()
buffer = io.BytesIO()
with tarfile.open(fileobj=buffer, mode="w", format=tarfile.USTAR_FORMAT) as archive:
    info = tarfile.TarInfo("hello.txt")
    info.size = len(payload)
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    archive.addfile(info, io.BytesIO(payload))
tar_bytes = buffer.getvalue()
diff = hashlib.sha256(tar_bytes).hexdigest()
layer = gzip.compress(tar_bytes, compresslevel=9, mtime=0)
layer_digest, layer_size = put(layer)
config = json.dumps(
    {
        "architecture": arch,
        "os": "linux",
        "config": {},
        "rootfs": {"type": "layers", "diff_ids": ["sha256:" + diff]},
    },
    separators=(",", ":"),
).encode()
config_digest, config_size = put(config)
manifest = json.dumps(
    {
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": "sha256:" + config_digest,
            "size": config_size,
        },
        "layers": [
            {
                "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                "digest": "sha256:" + layer_digest,
                "size": layer_size,
            }
        ],
    },
    separators=(",", ":"),
).encode()
manifest_digest, manifest_size = put(manifest)
index = json.dumps(
    {
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [
            {
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": "sha256:" + manifest_digest,
                "size": manifest_size,
                "annotations": {"org.opencontainers.image.ref.name": "stable"},
                "platform": {"architecture": arch, "os": "linux"},
            }
        ],
    },
    separators=(",", ":"),
).encode()
with open(os.path.join(root, "index.json"), "w", encoding="utf-8") as f:
    f.write(index.decode())
with open(os.path.join(root, "oci-layout"), "w", encoding="utf-8") as f:
    f.write('{"imageLayoutVersion":"1.0.0"}')
print(layer_digest)
PY
)
"$BIN" init --state-dir "$STATE" >/dev/null
"$BIN" import --state-dir "$STATE" --layout "$WORK/layout" --repository "$REPO" >/dev/null
"$BIN" publish --state-dir "$STATE" --site-dir "$SITE" >/dev/null
GEN=$(basename "$(readlink "$SITE/current")")
mkdir -p "$WORK/blobs/sha256"
for file in "$SITE"/blobs/sha256/*; do
  ln "$file" "$WORK/blobs/sha256/$(basename "$file")" 2>/dev/null || cp "$file" "$WORK/blobs/sha256/$(basename "$file")"
done
BLOB=$(python3 - <<'PYPORT'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PYPORT
)

python3 -m http.server "$BLOB" --bind 127.0.0.1 --directory "$WORK/blobs" > "$WORK/blob.log" 2>&1 &
BLOB_PID=$!
cat > "$WORK/redirect.mjs" <<'JS'
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [site, generation, blobOrigin, harnessPath] = process.argv.slice(2);
const { workerHarness } = await import(pathToFileURL(harnessPath));
const server = workerHarness({
  STUFF_OCI_HOST: "127.0.0.1",
  STUFF_OCI_BLOB_ORIGIN: blobOrigin,
  STUFF_ROUTES: "[]",
});
let stop;
const stopped = new Promise((resolve) => { stop = resolve; });
for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, stop);
try {
  const { url } = await server.listen();
  const meta = (await server.getWorker().getEnv()).OCI;
  await meta.put("current", generation);
  async function walk(dir, prefix) {
    for (const name of readdirSync(dir)) {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) {
        await walk(path, `${prefix}${name}/`);
      } else {
        await meta.put(`${prefix}${name}`, readFileSync(path));
      }
    }
  }
  await walk(join(site, "generations", generation), `generations/${generation}/`);
  await walk(join(site, "oci", "manifests"), "oci/manifests/");
  console.log(`READY ${url.port}`);
  await stopped;
} finally {
  await server.close();
}
JS

HARNESS=$ROOT/crates/stuff-worker/test/worker-harness.mjs
node "$WORK/redirect.mjs" "$SITE" "$GEN" "http://127.0.0.1:$BLOB" "$HARNESS" > "$WORK/front.log" 2>&1 &
FRONT_PID=$!
IMAGE=
cleanup() {
  status=$?
  trap - EXIT INT TERM
  kill "$FRONT_PID" "$BLOB_PID" >/dev/null 2>&1 || true
  wait "$FRONT_PID" "$BLOB_PID" 2>/dev/null || true
  [ -z "$IMAGE" ] || docker image rm -f "$IMAGE" >/dev/null 2>&1 || true
  if [ "$status" -eq 0 ] && [ -z "${KEEP:-}" ]; then
    rm -rf "$WORK"
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

tries=0
while ! rg -q "^READY [0-9]+$" "$WORK/front.log" 2>/dev/null; do
  kill -0 "$FRONT_PID" 2>/dev/null || fail "worker front exited"
  tries=$((tries + 1))
  [ "$tries" -lt 100 ] || fail "worker front did not start"
  sleep 0.1
done

FRONT=$(rg -o "^READY [0-9]+$" "$WORK/front.log" | cut -d " " -f 2)
IMAGE=127.0.0.1:$FRONT/$REPO:stable

docker pull -q "$IMAGE" >/dev/null || fail "docker pull through the redirect failed"
case "$(uname -m)" in
  arm64|aarch64) EXPECT=arm64 ;;
  x86_64|amd64) EXPECT=amd64 ;;
  *) fail "unsupported host architecture $(uname -m)" ;;
esac
GOT=$(docker image inspect --format '{{.Architecture}}' "$IMAGE")
[ "$GOT" = "$EXPECT" ] || fail "pulled $GOT, expected $EXPECT"
rg -q "GET /sha256/$LAYER" "$WORK/blob.log" || fail "the blob host did not see the redirect target"
echo "OK: Docker followed the 307 to the separate blob host (127.0.0.1:$BLOB)"
