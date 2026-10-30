#!/bin/sh
# Publish a pnpm lockfile's packages to our private npm registry and sync the
# site to R2. Called from the source repos' Makefiles, which own the lockfile
# and project name. Runs the whole flow in one shell under `set -eu` so a
# failed step aborts before any R2 write.
#
# Required env: LOCKFILE=<path to pnpm-lock.yaml> PROJECT=<project id>.
# Optional env (defaults shown):
#   STUFF_DIR=../stuff
#   PUBLIC_URL=https://npm.17166969.xyz UPSTREAM_REGISTRY=https://registry.npmjs.org/
#   BUCKET=stuff-mynpm STATE_DIR=.work/registry-pub/state SITE_DIR=.work/registry-pub/site
#   CONFIG_FILE=.work/registry-pub/mynpm.json
#   TARGETS="linux-x64-gnu" (extra space-separated target ids beyond the host
#   default, which is always imported; platform-specific entries merge)
set -eu
STUFF_DIR=${STUFF_DIR:-../stuff}
PUBLIC_URL=${PUBLIC_URL:-https://npm.17166969.xyz}
UPSTREAM_REGISTRY=${UPSTREAM_REGISTRY:-https://registry.npmjs.org/}
BUCKET=${BUCKET:-stuff-mynpm}
STATE_DIR=${STATE_DIR:-.work/registry-pub/state}
SITE_DIR=${SITE_DIR:-.work/registry-pub/site}
TARGETS=${TARGETS:-}
CONFIG_FILE=${CONFIG_FILE:-.work/registry-pub/mynpm.json}

MYNPM_BIN=$STUFF_DIR/target/debug/mynpm
R2_SYNC=$STUFF_DIR/deploy/r2-sync.sh

[ -n "${LOCKFILE:-}" ] || { echo "publish-mynpm: LOCKFILE is empty" >&2; exit 1; }
[ -n "${PROJECT:-}" ] || { echo "publish-mynpm: PROJECT is empty" >&2; exit 1; }

cargo build -p mynpm --manifest-path "$STUFF_DIR/Cargo.toml"
mkdir -p "$STATE_DIR" "$SITE_DIR" "$(dirname "$CONFIG_FILE")"
NODE_VERSION=$(node -e 'console.log(process.version.slice(1))')
"$MYNPM_BIN" init-config --node-version "$NODE_VERSION" --public-url "$PUBLIC_URL" --upstream-registry "$UPSTREAM_REGISTRY" --output "$CONFIG_FILE"
# The bare pass imports the host target; each TARGETS entry merges its
# platform-specific entries on top.
"$MYNPM_BIN" --config "$CONFIG_FILE" --state-dir "$STATE_DIR" import-lock "$LOCKFILE" --project "$PROJECT"
for t in $TARGETS; do
  "$MYNPM_BIN" --config "$CONFIG_FILE" --state-dir "$STATE_DIR" import-lock "$LOCKFILE" --project "$PROJECT" --target "$t"
done
"$MYNPM_BIN" --config "$CONFIG_FILE" --state-dir "$STATE_DIR" fetch
"$MYNPM_BIN" --config "$CONFIG_FILE" --state-dir "$STATE_DIR" publish --site-dir "$SITE_DIR"
# mynpm publish nests the servable tree under mirror/ (registry layout).
"$R2_SYNC" "$SITE_DIR/mirror" "$BUCKET"
