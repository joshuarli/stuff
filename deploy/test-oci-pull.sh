#!/bin/sh
# Publish the checked-in fixture, serve it with myoci, then pull through the
# read API with curl, Docker, and ORAS.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-"$ROOT/.work/oci-pull"}
TMPDIR=${TMPDIR:-"$ROOT/.work/tmp"}
export TMPDIR
mkdir -p "$TMPDIR"
rm -rf "$WORK"
mkdir -p "$WORK"

INDEX=sha256:04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388
ARM64=sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4
AMD64=sha256:d4ae9ea60e8f1f455de9959f23d711014d5f97e9d85ce82a6ecc769a61d1c65c
LAYER=sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f
REFERRER=sha256:3a91f0fb91009745e3a72651ec98d02a4c758c4f580bd9ab8683467f1f264f25
REFERRER_TYPE=application/vnd.stuff.sbom.v1+json

fail() {
  echo "FAIL: $1" >&2
  [ -f "$WORK/serve.log" ] && sed -n '1,40p' "$WORK/serve.log" >&2
  exit 1
}
expect() {
  [ "$1" = "$2" ] || fail "$3: expected $2, got $1"
}
assert_code() {
  jaq -e "$1" "$WORK/body" >/dev/null 2>&1 || {
    cat "$WORK/body" >&2
    fail "$2"
  }
}

command -v docker >/dev/null || fail "docker is required"
command -v curl >/dev/null || fail "curl is required"
command -v jaq >/dev/null || fail "jaq is required"
if ! command -v oras >/dev/null 2>&1; then
  if command -v go >/dev/null 2>&1; then
    GOBIN=${GOBIN:-"$(go env GOPATH)/bin"}
    if [ ! -x "$GOBIN/oras" ]; then
      (cd "$ROOT" && go install oras.land/oras/cmd/oras@v1.3.6)
    fi
    PATH="$GOBIN:$PATH"
    export PATH
  fi
fi
command -v oras >/dev/null || fail "oras 1.3.6 is required: go install oras.land/oras/cmd/oras@v1.3.6"

(cd "$ROOT" && cargo build -q -p myoci)
BIN=${CARGO_TARGET_DIR:-"$ROOT/target"}/debug/myoci
STATE=$WORK/state
SITE=$WORK/site
"$BIN" init --state-dir "$STATE" >/dev/null
"$BIN" import --state-dir "$STATE" --layout "$ROOT/tests/fixtures/oci/layout" --repository example/app --tag stable >/dev/null
"$BIN" tag --state-dir "$STATE" --repository example/app --tag beta --digest "$ARM64" >/dev/null
"$BIN" publish --state-dir "$STATE" --site-dir "$SITE" >/dev/null

"$BIN" serve --site-dir "$SITE" --bind 127.0.0.1:0 > "$WORK/serve.log" 2>&1 &
SERVER=$!
IMAGE_TAG=
IMAGE_DIGEST=
cleanup() {
  status=$?
  trap - EXIT INT TERM
  if [ -n "${SERVER:-}" ]; then
    kill "$SERVER" >/dev/null 2>&1 || true
    wait "$SERVER" 2>/dev/null || true
  fi
  if [ -n "${IMAGE_TAG:-}" ]; then
    docker image rm -f "$IMAGE_TAG" >/dev/null 2>&1 || true
  fi
  if [ -n "${IMAGE_DIGEST:-}" ]; then
    docker image rm -f "$IMAGE_DIGEST" >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

PORT=
i=0
while [ -z "$PORT" ] && [ "$i" -lt 100 ]; do
  PORT=$(sed -n 's/^listening on 127\.0\.0\.1://p' "$WORK/serve.log" 2>/dev/null)
  i=$((i + 1))
  [ -n "$PORT" ] || sleep 0.1
done
[ -n "$PORT" ] || fail "server did not start"
BASE=http://127.0.0.1:$PORT

# GET /v2/
CODE=$(curl -sS -o "$WORK/body" -D "$WORK/headers" -w '%{http_code}' "$BASE/v2/")
expect "$CODE" 200 "ping"
grep -qi '^docker-distribution-api-version: registry/2.0' "$WORK/headers" || fail "ping api version"

# HEAD manifest by tag
CODE=$(curl -sS -I -o "$WORK/headers" -w '%{http_code}' "$BASE/v2/example/app/manifests/stable")
expect "$CODE" 200 "HEAD manifest"
grep -qi '^content-type: application/vnd.oci.image.index.v1+json' "$WORK/headers" || fail "HEAD content-type"
grep -qi '^content-length: 491' "$WORK/headers" || fail "HEAD content-length"
grep -qi "^docker-content-digest: $INDEX" "$WORK/headers" || fail "HEAD digest"

# GET manifest by tag and digest
CODE=$(curl -sS -o "$WORK/body" -w '%{http_code}' "$BASE/v2/example/app/manifests/stable")
expect "$CODE" 200 "GET manifest by tag"
assert_code '.mediaType == "application/vnd.oci.image.index.v1+json"' "manifest media type"
CODE=$(curl -sS -o "$WORK/body" -w '%{http_code}' "$BASE/v2/example/app/manifests/$INDEX")
expect "$CODE" 200 "GET manifest by digest"
assert_code '.mediaType == "application/vnd.oci.image.index.v1+json"' "digest manifest media type"

# Range
CODE=$(curl -sS -o "$WORK/body" -D "$WORK/headers" -H 'Range: bytes=0-9' -w '%{http_code}' "$BASE/v2/example/app/blobs/$LAYER")
expect "$CODE" 206 "range"
grep -qi '^content-range: bytes 0-9/117' "$WORK/headers" || fail "range content-range"
head -c 10 "$ROOT/tests/fixtures/oci/layout/blobs/sha256/${LAYER#sha256:}" > "$WORK/expected"
cmp "$WORK/expected" "$WORK/body" >/dev/null || fail "range bytes"

# Tags pagination (beta and stable)
CODE=$(curl -sS -o "$WORK/body" -D "$WORK/headers" -w '%{http_code}' "$BASE/v2/example/app/tags/list?n=1")
expect "$CODE" 200 "tags page 1"
assert_code '.tags == ["beta"]' "tags page 1 body"
grep -qi '^link: </v2/example/app/tags/list?n=1&last=beta>; rel="next"' "$WORK/headers" || fail "tags link"
CODE=$(curl -sS -o "$WORK/body" -D "$WORK/headers" -w '%{http_code}' "$BASE/v2/example/app/tags/list?n=1&last=beta")
expect "$CODE" 200 "tags page 2"
assert_code '.tags == ["stable"]' "tags page 2 body"
grep -qi '^link:' "$WORK/headers" && fail "unexpected second-page link"

# Referrers, with and without artifactType
CODE=$(curl -sS -o "$WORK/body" -w '%{http_code}' "$BASE/v2/example/app/referrers/$ARM64")
expect "$CODE" 200 "referrers"
assert_code "(.manifests | length) == 1 and .manifests[0].digest == \"$REFERRER\"" "referrers body"
CODE=$(curl -sS -o "$WORK/body" -D "$WORK/headers" -w '%{http_code}' "$BASE/v2/example/app/referrers/$ARM64?artifactType=$(printf '%s' "$REFERRER_TYPE" | sed 's#/#%2F#; s#+#%2B#')")
expect "$CODE" 200 "filtered referrers"
grep -qi '^oci-filters-applied: artifactType' "$WORK/headers" || fail "filters applied header"
assert_code "(.manifests | length) == 1 and .manifests[0].digest == \"$REFERRER\"" "filtered referrers body"
CODE=$(curl -sS -o "$WORK/body" -w '%{http_code}' "$BASE/v2/example/app/referrers/$ARM64?artifactType=application%2Fvnd.example.unknown")
expect "$CODE" 200 "empty referrers"
assert_code '.manifests == []' "empty referrers body"

# Errors
CODE=$(curl -sS -o "$WORK/body" -w '%{http_code}' "$BASE/v2/example/app/manifests/nope")
expect "$CODE" 404 "unknown manifest"
assert_code '.errors[0].code == "MANIFEST_UNKNOWN"' "unknown manifest code"
CODE=$(curl -sS -o "$WORK/body" -w '%{http_code}' "$BASE/v2/example/missing/tags/list")
expect "$CODE" 404 "unknown repository"
assert_code '.errors[0].code == "NAME_UNKNOWN"' "unknown repository code"
CODE=$(curl -sS -o "$WORK/body" -w '%{http_code}' "$BASE/v2/example/app/blobs/sha256:00")
expect "$CODE" 400 "invalid digest"
assert_code '.errors[0].code == "DIGEST_INVALID"' "invalid digest code"
CODE=$(curl -sS -o "$WORK/body" -w '%{http_code}' "$BASE/v2/Example/app/manifests/stable")
expect "$CODE" 400 "invalid name"
assert_code '.errors[0].code == "NAME_INVALID"' "invalid name code"
CODE=$(curl -sS -o "$WORK/body" -D "$WORK/headers" -X POST -w '%{http_code}' "$BASE/v2/example/app/blobs/uploads/")
expect "$CODE" 405 "write method"
grep -qi '^allow: GET, HEAD' "$WORK/headers" || fail "405 allow header"
assert_code '.errors[0].code == "UNSUPPORTED"' "405 code"

# Docker pulls by tag and digest
REGISTRY=127.0.0.1:$PORT
IMAGE_TAG=$REGISTRY/example/app:stable
IMAGE_DIGEST=$REGISTRY/example/app@$INDEX
docker pull -q "$IMAGE_TAG" >/dev/null || fail "docker pull by tag"
case "$(uname -m)" in
  arm64|aarch64) EXPECT_ARCH=arm64 ;;
  x86_64|amd64) EXPECT_ARCH=amd64 ;;
  *) fail "unsupported host architecture $(uname -m)" ;;
esac
GOT_ARCH=$(docker image inspect --format '{{.Architecture}}' "$IMAGE_TAG")
expect "$GOT_ARCH" "$EXPECT_ARCH" "docker tag architecture"
docker pull -q "$IMAGE_DIGEST" >/dev/null || fail "docker pull by digest"
GOT_ARCH=$(docker image inspect --format '{{.Architecture}}' "$IMAGE_DIGEST")
expect "$GOT_ARCH" "$EXPECT_ARCH" "docker digest architecture"
[ "$EXPECT_ARCH" = arm64 ] || echo "note: host is $EXPECT_ARCH; the arm64 child was not selected here"

# ORAS
DESCRIPTOR=$(oras manifest fetch --plain-http --descriptor "$REGISTRY/example/app:stable")
printf '%s' "$DESCRIPTOR" | jaq -e --arg digest "$INDEX" '.digest == $digest' >/dev/null || fail "oras manifest descriptor"
oras manifest fetch --plain-http "$REGISTRY/example/app:stable" | jaq -e '.mediaType == "application/vnd.oci.image.index.v1+json"' >/dev/null || fail "oras manifest body"
oras blob fetch --plain-http --output "$WORK/blob" "$REGISTRY/example/app@$LAYER" >/dev/null || fail "oras blob fetch"
[ "$(wc -c < "$WORK/blob" | tr -d ' ')" = 117 ] || fail "oras blob size"
[ "$(shasum -a 256 "$WORK/blob" 2>/dev/null | cut -d' ' -f1 || sha256sum "$WORK/blob" | cut -d' ' -f1)" = "${LAYER#sha256:}" ] || fail "oras blob digest"

echo "OK: read API, Docker pulls, and ORAS pulls are green on $REGISTRY"
