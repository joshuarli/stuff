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
#   BUCKET=stuff-mynpm
#   STUFF_STATE_HOME=${XDG_STATE_HOME:-$HOME/.local/state}/stuff
#   STATE_DIR=$STUFF_STATE_HOME/mynpm/state SITE_DIR=$STUFF_STATE_HOME/mynpm/site
#   CONFIG_FILE=$STUFF_STATE_HOME/mynpm/mynpm.json
#   LOCK_DIR=$STUFF_STATE_HOME/mynpm/publish.lock
#   TARGETS="linux-x64-gnu" (extra space-separated target ids beyond the host
#   default, which is always imported; platform-specific entries merge)
#
# State lives outside the calling checkout and the whole transaction is
# serialized, so publishing from another source repository extends one index.
set -eu
STUFF_DIR=${STUFF_DIR:-../stuff}
PUBLIC_URL=${PUBLIC_URL:-https://npm.17166969.xyz}
UPSTREAM_REGISTRY=${UPSTREAM_REGISTRY:-https://registry.npmjs.org/}
BUCKET=${BUCKET:-stuff-mynpm}
STUFF_STATE_HOME=${STUFF_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/stuff}
STATE_DIR=${STATE_DIR:-$STUFF_STATE_HOME/mynpm/state}
SITE_DIR=${SITE_DIR:-$STUFF_STATE_HOME/mynpm/site}
TARGETS=${TARGETS:-}
CONFIG_FILE=${CONFIG_FILE:-$STUFF_STATE_HOME/mynpm/mynpm.json}
LOCK_DIR=${LOCK_DIR:-$STUFF_STATE_HOME/mynpm/publish.lock}

. "$STUFF_DIR/deploy/publish-lock.sh"
LEGACY_STATE_DIR=${LEGACY_STATE_DIR:-$PWD/.work/registry-pub/state}
if { [ ! -d "$STATE_DIR" ] || [ -z "$(ls -A "$STATE_DIR" 2>/dev/null)" ]; } && [ -d "$LEGACY_STATE_DIR" ] && [ "$STATE_DIR" != "$LEGACY_STATE_DIR" ]; then
  echo "publish-mynpm: legacy per-checkout state exists at $LEGACY_STATE_DIR" >&2
  echo "publish-mynpm: move it to the canonical $STATE_DIR (or set STATE_DIR explicitly) so another repository cannot hide its packages" >&2
  exit 1
fi
mkdir -p "$STUFF_STATE_HOME/mynpm"
acquire_publish_lock "$LOCK_DIR"
trap 'release_publish_lock' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

MYNPM_BIN=$STUFF_DIR/target/debug/mynpm
R2_SYNC=$STUFF_DIR/deploy/r2-sync.sh
PRUNE=$STUFF_DIR/deploy/prune-generations.sh

if [ -n "${REPUBLISH:-}" ]; then
  cargo build -p mynpm --manifest-path "$STUFF_DIR/Cargo.toml"
  "$MYNPM_BIN" adopt --site-dir "$SITE_DIR"
  "$R2_SYNC" "$SITE_DIR/mirror" "$BUCKET"
  "$PRUNE" "$SITE_DIR/mirror"
  exit 0
fi

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
"$PRUNE" "$SITE_DIR/mirror"
