#!/bin/sh
# Benchmark the OCI read API and rewrite docs/oci-benchmarks.md.
# With no --base-url, build a loopback fixture with 1 MiB, 64 MiB, 256 MiB,
# and 1 GiB layers and serve it with myoci serve.
# usage: deploy/oci-bench.sh [--base-url URL] [--repo NAME] [--tag TAG] [--runs N]
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${OCI_BENCH_WORK:-"$ROOT/.work/oci-bench"}
RUNS=${RUNS:-5}
BASE_URL=""
REPO=${OCI_BENCH_REPO:-example/bench}
TAG=${OCI_BENCH_TAG:-stable}
while [ $# -gt 0 ]; do
  case $1 in
    --base-url) [ $# -ge 2 ] || exit 2; BASE_URL=$2; shift 2 ;;
    --repo) [ $# -ge 2 ] || exit 2; REPO=$2; shift 2 ;;
    --tag) [ $# -ge 2 ] || exit 2; TAG=$2; shift 2 ;;
    --runs) [ $# -ge 2 ] || exit 2; RUNS=$2; shift 2 ;;
    *) echo "usage: deploy/oci-bench.sh [--base-url URL] [--repo NAME] [--tag TAG] [--runs N]" >&2; exit 2 ;;
  esac
done

(cd "$ROOT" && cargo build -q -p myoci)
BIN=${CARGO_TARGET_DIR:-"$ROOT/target"}/debug/myoci
OUTPUT=$ROOT/docs/oci-benchmarks.md

if [ -n "$BASE_URL" ]; then
  "$BIN" bench --base-url "$BASE_URL" --repo "$REPO" --tag "$TAG" \
    --runs "$RUNS" --output "$OUTPUT" --label "existing registry"
  exit 0
fi

rm -rf "$WORK"
mkdir -p "$WORK"
python3 - "$WORK/layout" <<'PY'
import hashlib
import json
import os
import sys

root = sys.argv[1]
blobs = os.path.join(root, "blobs", "sha256")
os.makedirs(blobs, exist_ok=True)
sizes = [1 << 20, 64 << 20, 256 << 20, 1 << 30]
pattern = bytes(range(256)) * 4096


def write_blob(data):
    digest = hashlib.sha256(data).hexdigest()
    with open(os.path.join(blobs, digest), "wb") as f:
        f.write(data)
    return digest, len(data)


def write_file_blob(path):
    digest = hashlib.sha256()
    size = 0
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
            size += len(chunk)
    hexdigest = digest.hexdigest()
    os.rename(path, os.path.join(blobs, hexdigest))
    return hexdigest, size


config_digest, config_size = write_blob(b"{}")
layers = []
for index, size in enumerate(sizes):
    path = os.path.join(root, f"layer-{index}")
    with open(path, "wb") as f:
        remaining = size
        while remaining:
            chunk = pattern[: min(len(pattern), remaining)]
            f.write(chunk)
            remaining -= len(chunk)
    digest, written = write_file_blob(path)
    layers.append(
        {
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": "sha256:" + digest,
            "size": written,
        }
    )

children = []
for platform in ({"architecture": "arm64", "os": "linux"}, {"architecture": "amd64", "os": "linux"}):
    manifest = {
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": "sha256:" + config_digest,
            "size": config_size,
        },
        "layers": layers,
    }
    body = json.dumps(manifest, separators=(",", ":")).encode()
    digest, size = write_blob(body)
    children.append(
        {
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": "sha256:" + digest,
            "size": size,
            "platform": platform,
        }
    )

index = {
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.index.v1+json",
    "manifests": children,
}
index_body = json.dumps(index, separators=(",", ":")).encode()
index_digest, index_size = write_blob(index_body)
layout_index = {
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.index.v1+json",
    "manifests": [
        {
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "digest": "sha256:" + index_digest,
            "size": index_size,
            "annotations": {"org.opencontainers.image.ref.name": "stable"},
        }
    ],
}
with open(os.path.join(root, "index.json"), "w", encoding="utf-8") as f:
    json.dump(layout_index, f, separators=(",", ":"))
with open(os.path.join(root, "oci-layout"), "w", encoding="utf-8") as f:
    json.dump({"imageLayoutVersion": "1.0.0"}, f, separators=(",", ":"))
PY

"$BIN" init --state-dir "$WORK/state" >/dev/null
"$BIN" import --state-dir "$WORK/state" --layout "$WORK/layout" --repository "$REPO" --tag "$TAG" >/dev/null
"$BIN" publish --state-dir "$WORK/state" --site-dir "$WORK/site" >/dev/null
rm -rf "$WORK/layout"

"$BIN" serve --site-dir "$WORK/site" --bind 127.0.0.1:0 > "$WORK/serve.log" 2>&1 &
SERVER=$!
cleanup() {
  status=$?
  trap - EXIT INT TERM
  kill "$SERVER" >/dev/null 2>&1 || true
  wait "$SERVER" 2>/dev/null || true
  if [ "$status" -eq 0 ] && [ -z "${KEEP:-}" ]; then
    rm -rf "$WORK"
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

PORT=""
tries=0
while [ -z "$PORT" ] && [ "$tries" -lt 100 ]; do
  PORT=$(sed -n 's/^listening on 127\.0\.0\.1://p' "$WORK/serve.log" 2>/dev/null)
  tries=$((tries + 1))
  [ -n "$PORT" ] || sleep 0.1
done
[ -n "$PORT" ] || {
  echo "bench: myoci serve did not start" >&2
  cat "$WORK/serve.log" >&2
  exit 1
}
"$BIN" bench --base-url "http://127.0.0.1:$PORT" --repo "$REPO" --tag "$TAG" \
  --runs "$RUNS" --output "$OUTPUT" --label "myoci serve (loopback)"
