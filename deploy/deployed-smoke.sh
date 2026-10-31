#!/bin/sh
# Read-only smoke test against the DEPLOYED Cloudflare registries.
# Usage: deploy/deployed-smoke.sh
# Env: STUFF_CARGO_BASE / STUFF_NPM_BASE / STUFF_PYPI_BASE override the
# default https://{crates,npm,pypi}.17166969.xyz origins (stuff honors the
# same variables). NPM_REGISTRY / PYPI_INDEX_URL override the
# package-manager URLs similarly.
#
# Checks the active generations, catalog objects, essential endpoints and
# sample artifact hashes (no Cloudflare credentials needed), then probes
# each non-empty registry with its real package manager in read-only mode.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)

NPM_REGISTRY=${NPM_REGISTRY:-${STUFF_NPM_BASE:-https://npm.17166969.xyz}/current}
PYPI_INDEX_URL=${PYPI_INDEX_URL:-${STUFF_PYPI_BASE:-https://pypi.17166969.xyz}/current/simple/}

fail() { echo "FAIL: $1"; exit 1; }

echo "--- stuff doctor --remote --sample-artifacts"
"$ROOT/target/debug/stuff" doctor --remote --sample-artifacts || fail "remote doctor checks failed"

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

echo "OK: deployed registries answer through HTTP and their package managers"
