#!/bin/sh
# Read-only smoke test against the DEPLOYED Cloudflare registries.
# Usage: deploy/deployed-smoke.sh
# Env: STUFF_CARGO_BASE / STUFF_NPM_BASE / STUFF_PYPI_BASE / STUFF_OCI_BASE
# override the default https://{crates,npm,pypi,oci}.17166969.xyz origins (stuff honors the
# same variables). NPM_REGISTRY / PYPI_INDEX_URL override the
# package-manager URLs similarly. STUFF_MIRROR_BASE overrides the static
# artifact mirror origin (default https://mirror.17166969.xyz).
#
# Checks the active generations, catalog objects, essential endpoints and
# sample artifact hashes (no Cloudflare credentials needed), then probes
# each non-empty registry with its real package manager in read-only mode.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)

NPM_REGISTRY=${NPM_REGISTRY:-${STUFF_NPM_BASE:-https://npm.17166969.xyz}/current}
PYPI_INDEX_URL=${PYPI_INDEX_URL:-${STUFF_PYPI_BASE:-https://pypi.17166969.xyz}/current/simple/}

fail() { echo "FAIL: $1"; exit 1; }

echo "--- stuff doctor --sample-artifacts"
"$ROOT/target/debug/stuff" doctor --sample-artifacts || fail "remote doctor checks failed"

first_pkg() {
  json=$("$ROOT/target/debug/stuff" ls --registry "$1" --json) || fail "stuff ls --registry $1 failed"
  printf '%s\n' "$json" |
    jaq -r '.catalogs[0].packages[0] | if . == null then "null null" else "\(.name) \(.latest)" end' ||
    fail "could not parse stuff ls output for $1"
}

NPM_PKG=$(first_pkg npm)
if [ -n "$NPM_PKG" ] && [ "$NPM_PKG" != "null null" ]; then
  set -- $NPM_PKG
  echo "--- npm view $1@$2 from $NPM_REGISTRY"
  npm view "$1@$2" version --registry "$NPM_REGISTRY" || fail "npm view failed"
else
  echo "skip: npm registry is empty"
fi

PYPI_PKG=$(first_pkg pypi)
if [ -n "$PYPI_PKG" ] && [ "$PYPI_PKG" != "null null" ]; then
  set -- $PYPI_PKG
  echo "--- uv pip install --dry-run $1==$2 from $PYPI_INDEX_URL"
  uv pip install --dry-run --system --no-deps --python 3.15 --index-url "$PYPI_INDEX_URL" "$1==$2" >/dev/null || fail "uv pip install --dry-run failed"
else
  echo "skip: pypi registry is empty"
fi

OCI_HOST=$(printf '%s' "${STUFF_OCI_BASE:-https://oci.17166969.xyz}" | sed 's|^https*://||; s|/*$||')
OCI_PKG=$(first_pkg oci)
if [ -n "$OCI_PKG" ] && [ "$OCI_PKG" != "null null" ]; then
  set -- $OCI_PKG
  echo "--- oras manifest fetch $OCI_HOST/$1:$2"
  oras manifest fetch --descriptor "$OCI_HOST/$1:$2" >/dev/null || fail "oras manifest fetch failed"
else
  echo "skip: oci registry is empty"
fi

MIRROR_BASE=${STUFF_MIRROR_BASE:-https://mirror.17166969.xyz}
echo "--- mirror SHA256SUMS and one artifact"
sums=$(curl -fsS "$MIRROR_BASE/current/SHA256SUMS") || fail "mirror SHA256SUMS fetch failed"
line=$(printf '%s\n' "$sums" | sed -n 1p)
case "$line" in
  *"  "*.zip) ;;
  *) fail "mirror SHA256SUMS line is malformed: $line" ;;
esac
set -- $line
want_sha=$1
name=$2
case "$want_sha" in
  *[!0-9a-f]*) fail "mirror SHA256SUMS hash is malformed: $line" ;;
esac
case "$name" in
  */*) fail "mirror artifact name has a path separator: $name" ;;
esac
mirror_tmp=$(mktemp -d)
trap 'rm -rf "$mirror_tmp"' EXIT INT TERM
curl -fsS -o "$mirror_tmp/$name" "$MIRROR_BASE/current/$name" || fail "mirror artifact $name fetch failed"
if command -v sha256sum >/dev/null 2>&1; then
  actual_sha=$(sha256sum "$mirror_tmp/$name" | cut -d' ' -f1)
else
  actual_sha=$(shasum -a 256 "$mirror_tmp/$name" | cut -d' ' -f1)
fi
[ "$actual_sha" = "$want_sha" ] || fail "mirror artifact $name hash mismatch"
rm -rf "$mirror_tmp"
trap - EXIT INT TERM
echo "mirror artifact $name matches SHA256SUMS"

echo "OK: deployed registries answer through HTTP and their package managers"
