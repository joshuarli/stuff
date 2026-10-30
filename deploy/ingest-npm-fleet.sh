#!/bin/sh
# Ingest every pnpm lockfile listed in deploy/npm-fleet.txt into one mynpm
# state directory, fetch the artifacts, and publish a local site. Never writes
# to R2; sync the site afterwards with deploy/r2-sync.sh.
#
# Re-running is safe: an unchanged lockfile and project snapshot merge as a
# no-op, a changed one is recorded as an additional import of the same project
# id (already hosted versions stay), and fetch reuses verified artifacts.
#
# Env (defaults shown):
#   STUFF_DIR=<this repo>  REPOS_DIR=$HOME/repos  FLEET_FILE=deploy/npm-fleet.txt
#   PUBLIC_URL=https://npm.17166969.xyz UPSTREAM_REGISTRY=https://registry.npmjs.org/
#   STATE_DIR=.work/npm-fleet/state SITE_DIR=.work/npm-fleet/site
#   CONFIG_FILE=.work/npm-fleet/mynpm.json
# Targets are the six darwin/linux platforms of `init-config --all-targets`.
set -eu
STUFF_DIR=${STUFF_DIR:-$(cd "$(dirname "$0")/.." && pwd)}
REPOS_DIR=${REPOS_DIR:-$HOME/repos}
FLEET_FILE=${FLEET_FILE:-$STUFF_DIR/deploy/npm-fleet.txt}
PUBLIC_URL=${PUBLIC_URL:-https://npm.17166969.xyz}
UPSTREAM_REGISTRY=${UPSTREAM_REGISTRY:-https://registry.npmjs.org/}
STATE_DIR=${STATE_DIR:-$PWD/.work/npm-fleet/state}
SITE_DIR=${SITE_DIR:-$PWD/.work/npm-fleet/site}
CONFIG_FILE=${CONFIG_FILE:-$PWD/.work/npm-fleet/mynpm.json}

cargo build -p mynpm --manifest-path "$STUFF_DIR/Cargo.toml"
MYNPM_BIN=$STUFF_DIR/target/debug/mynpm
mkdir -p "$STATE_DIR" "$SITE_DIR" "$(dirname "$CONFIG_FILE")"
NODE_VERSION=$(node -e 'console.log(process.version.slice(1))')
"$MYNPM_BIN" init-config --node-version "$NODE_VERSION" --all-targets --public-url "$PUBLIC_URL" --upstream-registry "$UPSTREAM_REGISTRY" --output "$CONFIG_FILE"

TARGET_ARGS=""
for id in $(jaq -r '.targets[].id' "$CONFIG_FILE"); do
  TARGET_ARGS="$TARGET_ARGS --target $id"
done

while read -r lockfile project; do
  [ -n "$lockfile" ] || continue
  dir=$REPOS_DIR/$(dirname "$lockfile")
  # shellcheck disable=SC2086
  (cd "$dir" && "$MYNPM_BIN" --config "$CONFIG_FILE" --state-dir "$STATE_DIR" import-lock "$(basename "$lockfile")" --project "$project" $TARGET_ARGS)
done < "$FLEET_FILE"

"$MYNPM_BIN" --config "$CONFIG_FILE" --state-dir "$STATE_DIR" fetch
"$MYNPM_BIN" --config "$CONFIG_FILE" --state-dir "$STATE_DIR" publish --site-dir "$SITE_DIR"
