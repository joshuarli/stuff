#!/bin/sh
# Import a pinned multi-platform image through oras into a local state and
# assert the recorded graph. Docker Hub is rate limited on some hosts; set
# OCI_IMPORT_SAMPLE to the same digest on a mirror to exercise the flow.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-"$ROOT/.work/oci-import"}
SAMPLE=${OCI_IMPORT_SAMPLE:-docker.io/library/alpine@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc}
INDEX=${OCI_IMPORT_INDEX:-sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc}
TMPDIR=${TMPDIR:-"$ROOT/.work/tmp"}
export TMPDIR
mkdir -p "$TMPDIR"
rm -rf "$WORK"
mkdir -p "$WORK"

command -v jaq >/dev/null || {
  echo "FAIL: jaq is required" >&2
  exit 1
}
if ! command -v oras >/dev/null 2>&1; then
  command -v go >/dev/null 2>&1 || {
    echo "FAIL: oras 1.3.6 is required: go install oras.land/oras/cmd/oras@v1.3.6" >&2
    exit 1
  }
  GOBIN=${GOBIN:-"$(go env GOPATH)/bin"}
  if [ ! -x "$GOBIN/oras" ]; then
    (cd "$ROOT" && go install oras.land/oras/cmd/oras@v1.3.6)
  fi
  PATH="$GOBIN:$PATH"
  export PATH
fi

STATE=$WORK/state
(cd "$ROOT" && cargo run -q -p myoci -- init --state-dir "$STATE")
(cd "$ROOT" && cargo run -q -p myoci -- import --state-dir "$STATE" --from "$SAMPLE" --repository example/alpine --tag stable)
(cd "$ROOT" && cargo run -q -p myoci -- inspect --state-dir "$STATE" --json > "$WORK/inspect.json")

if ! jaq -e --arg index "$INDEX" \
  '.tags == ["stable"] and (.manifests | index($index) != null) and (.blobs | length > 0)' \
  "$WORK/inspect.json" >/dev/null; then
  echo "FAIL: import inventory does not contain $INDEX" >&2
  cat "$WORK/inspect.json" >&2
  exit 1
fi
MANIFESTS=$(jaq -r '.manifests | length' "$WORK/inspect.json")
BLOBS=$(jaq -r '.blobs | length' "$WORK/inspect.json")
echo "OK: imported $SAMPLE ($MANIFESTS manifests, $BLOBS blobs)"
