#!/bin/sh
# Run the pinned OCI Distribution 1.1 conformance suite.
#
# The suite is cloned and built under .work/tools. The checked-in patch guards
# the read-only report writer against status keys that have no data entry; apply
# it with `git apply --check` first so an upstream change stops the gate instead
# of hiding a regression.
#
# Usage:
#   deploy/oci-conformance.sh --fixture-layout tests/fixtures/oci/layout
#   deploy/oci-conformance.sh --myoci-state DIR --myoci-site DIR
#   OCI_REGISTRY=host:port OCI_TLS=enabled deploy/oci-conformance.sh \
#     --tag stable --manifest sha256:... --blob sha256:... --referrer sha256:...
#   deploy/oci-conformance.sh --fixture-layout DIR --push
#
# Fixture mode starts deploy/oci-fixture-registry.py, fills OCI_RO_DATA_* from
# its --inspect output, and stops it. Myoci mode starts `myoci serve` on the
# published site and fills OCI_RO_DATA_* from `myoci inspect --json`. Direct
# mode reads OCI_* from the environment (or the flags above) and does not start
# a server.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
REV=97274622c11112caa21efb8c52acca3c6b8fa7f1
SRC="$ROOT/.work/tools/distribution-spec"
BIN="$ROOT/.work/tools/oci-conformance"
TMPDIR="$ROOT/.work/tmp"
export TMPDIR
mkdir -p "$TMPDIR" "$ROOT/.work/tools"

usage() {
  cat >&2 <<'EOF'
usage: deploy/oci-conformance.sh [--fixture-layout DIR] [--push] [--results DIR]
       [--myoci-state DIR --myoci-site DIR]
       [--registry HOST] [--tls MODE] [--tag TAG] [--manifest DIGEST]...
       [--blob DIGEST]... [--referrer DIGEST]...
EOF
  exit 2
}

FIXTURE=""
MYOCI_STATE=""
MYOCI_SITE=""
PUSH=""
RESULTS="$ROOT/.work/oci-conformance/results"
TAGS="${OCI_RO_DATA_TAGS:-}"
MANIFESTS="${OCI_RO_DATA_MANIFESTS:-}"
BLOBS="${OCI_RO_DATA_BLOBS:-}"
REFERRERS="${OCI_RO_DATA_REFERRERS:-}"
REGISTRY="${OCI_REGISTRY:-}"
TLS="${OCI_TLS:-disabled}"
while [ $# -gt 0 ]; do
  case $1 in
    --fixture-layout) [ $# -ge 2 ] || usage; FIXTURE=$2; shift 2 ;;
    --myoci-state) [ $# -ge 2 ] || usage; MYOCI_STATE=$2; shift 2 ;;
    --myoci-site) [ $# -ge 2 ] || usage; MYOCI_SITE=$2; shift 2 ;;
    --push) PUSH=1; shift ;;
    --results) [ $# -ge 2 ] || usage; RESULTS=$2; shift 2 ;;
    --registry) [ $# -ge 2 ] || usage; REGISTRY=$2; shift 2 ;;
    --tls) [ $# -ge 2 ] || usage; TLS=$2; shift 2 ;;
    --tag) [ $# -ge 2 ] || usage; TAGS="$TAGS $2"; shift 2 ;;
    --manifest) [ $# -ge 2 ] || usage; MANIFESTS="$MANIFESTS $2"; shift 2 ;;
    --blob) [ $# -ge 2 ] || usage; BLOBS="$BLOBS $2"; shift 2 ;;
    --referrer) [ $# -ge 2 ] || usage; REFERRERS="$REFERRERS $2"; shift 2 ;;
    -h | --help) usage ;;
    *) usage ;;
  esac
done

for tool in git go jaq python3; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "conformance: $tool is required" >&2
    exit 1
  }
done

ORACLE_PID=""
cleanup() {
  if [ -n "$ORACLE_PID" ]; then
    kill "$ORACLE_PID" 2>/dev/null || true
    wait "$ORACLE_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

if [ -n "$FIXTURE" ]; then
  [ -z "$MYOCI_STATE" ] && [ -z "$MYOCI_SITE" ] || usage
  LOG="$TMPDIR/oci-fixture-registry.log"
  python3 "$ROOT/deploy/oci-fixture-registry.py" --root "$FIXTURE" --port 0 > "$LOG" 2>&1 &
  ORACLE_PID=$!
  PORT=""
  tries=0
  while [ "$tries" -lt 100 ]; do
    PORT=$(sed -n 's/^PORT=//p' "$LOG" | head -1)
    [ -n "$PORT" ] && break
    tries=$((tries + 1))
    sleep 0.1
  done
  [ -n "$PORT" ] || { echo "conformance: fixture registry did not start" >&2; cat "$LOG" >&2; exit 1; }
  REGISTRY="127.0.0.1:$PORT"
  TLS=disabled
  INSPECT=$(python3 "$ROOT/deploy/oci-fixture-registry.py" --root "$FIXTURE" --inspect)
  TAGS=$(printf '%s' "$INSPECT" | jaq -r '.tags | join(" ")')
  MANIFESTS=$(printf '%s' "$INSPECT" | jaq -r '.manifests | join(" ")')
  BLOBS=$(printf '%s' "$INSPECT" | jaq -r '.blobs | join(" ")')
  REFERRERS=$(printf '%s' "$INSPECT" | jaq -r '.referrers | join(" ")')
elif [ -n "$MYOCI_STATE" ] || [ -n "$MYOCI_SITE" ]; then
  [ -n "$MYOCI_STATE" ] && [ -n "$MYOCI_SITE" ] || usage
  [ -d "$MYOCI_SITE" ] || { echo "conformance: $MYOCI_SITE is not a published site" >&2; exit 1; }
  (cd "$ROOT" && cargo build -q -p myoci)
  MYOCI_BIN=${CARGO_TARGET_DIR:-"$ROOT/target"}/debug/myoci
  LOG="$TMPDIR/oci-myoci-serve.log"
  "$MYOCI_BIN" serve --site-dir "$MYOCI_SITE" --bind 127.0.0.1:0 > "$LOG" 2>&1 &
  ORACLE_PID=$!
  PORT=""
  tries=0
  while [ "$tries" -lt 100 ]; do
    PORT=$(sed -n 's/^listening on 127\.0\.0\.1://p' "$LOG" | head -1)
    [ -n "$PORT" ] && break
    tries=$((tries + 1))
    sleep 0.1
  done
  [ -n "$PORT" ] || { echo "conformance: myoci did not start" >&2; cat "$LOG" >&2; exit 1; }
  REGISTRY="127.0.0.1:$PORT"
  TLS=disabled
  INSPECT=$("$MYOCI_BIN" inspect --state-dir "$MYOCI_STATE" --json)
  TAGS=$(printf '%s' "$INSPECT" | jaq -r '.tags | join(" ")')
  MANIFESTS=$(printf '%s' "$INSPECT" | jaq -r '.manifests | join(" ")')
  BLOBS=$(printf '%s' "$INSPECT" | jaq -r '.blobs | join(" ")')
  REFERRERS=$(printf '%s' "$INSPECT" | jaq -r '.referrers | join(" ")')
fi

[ -n "$REGISTRY" ] || usage

if [ ! -d "$SRC/.git" ]; then
  echo "conformance: cloning distribution-spec $REV" >&2
  git clone --quiet https://github.com/opencontainers/distribution-spec "$SRC"
fi
git -C "$SRC" checkout --quiet --force "$REV"
git -C "$SRC" clean -qfd
if ! git -C "$SRC" apply --check "$ROOT/deploy/oci-conformance-report.patch" 2>/dev/null; then
  echo "conformance: deploy/oci-conformance-report.patch does not apply at $REV" >&2
  echo "conformance: inspect the upstream report writer before changing the gate" >&2
  exit 1
fi
git -C "$SRC" apply "$ROOT/deploy/oci-conformance-report.patch"
echo "conformance: building $REV" >&2
(cd "$SRC/conformance" && GOFLAGS=-buildvcs=false go build -o "$BIN" .)

rm -rf "$RESULTS"
mkdir -p "$RESULTS"
OCI_RESULTS_DIR=$RESULTS
OCI_VERSION=1.1
OCI_LOG=warn
OCI_REGISTRY=$REGISTRY
OCI_TLS=$TLS
OCI_REPO1=${OCI_REPO1:-example/app}
OCI_RO_DATA_TAGS=$TAGS
OCI_RO_DATA_MANIFESTS=$MANIFESTS
OCI_RO_DATA_BLOBS=$BLOBS
OCI_RO_DATA_REFERRERS=$REFERRERS
if [ -n "$PUSH" ]; then
  OCI_API_PUSH=true
else
  OCI_API_PUSH=false
fi
OCI_API_BLOBS_DELETE=false
OCI_API_MANIFESTS_DELETE=false
OCI_API_TAGS_DELETE=false
OCI_API_TAGS_LIST=true
OCI_API_REFERRER=true
export OCI_RESULTS_DIR OCI_VERSION OCI_LOG OCI_REGISTRY OCI_TLS OCI_REPO1
export OCI_RO_DATA_TAGS OCI_RO_DATA_MANIFESTS OCI_RO_DATA_BLOBS OCI_RO_DATA_REFERRERS
export OCI_API_PUSH OCI_API_BLOBS_DELETE OCI_API_MANIFESTS_DELETE OCI_API_TAGS_DELETE
export OCI_API_TAGS_LIST OCI_API_REFERRER

set +e
"$BIN"
STATUS=$?
set -e

if [ ! -f "$RESULTS/results.yaml" ]; then
  echo "conformance: no results.yaml; the suite did not report" >&2
  exit 1
fi
BAD=$(jaq -r --from yaml '
  [(.apis // {}), (.data // {})] | add
  | to_entries[]
  | select(.value != "Pass" and .value != "Disabled")
  | "\(.key): \(.value)"
' "$RESULTS/results.yaml")
COUNT=$(jaq -r --from yaml '[(.apis // {}), (.data // {})] | add | length' "$RESULTS/results.yaml")
if [ -n "$BAD" ]; then
  echo "conformance: FAIL" >&2
  printf '%s\n' "$BAD" >&2
  exit 1
fi
echo "conformance: ok ($COUNT rows, $REGISTRY)"
exit "$STATUS"
