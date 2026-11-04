#!/bin/sh
# Publish crates to our private mycrates registry and sync the site to R2.
# Called from the source repos' Makefiles (smolith, h12tiny), which own their
# crate lists. Runs the whole flow in one shell under `set -eu` with server
# cleanup on exit, so a failed `cargo publish` aborts before any R2 write.
# One loopback `mycrates serve-publish` process takes the Cargo write API and
# serves the sparse index and blobs; nothing is compiled except by `cargo
# publish` itself.
# The exit trap restores PUBLIC_URL and rebuilds the site, so an aborted run
# never leaves a localhost base URL for a later manual r2-sync to promote.
#
# Required env: CRATES="name-a name-b ..." (space-separated package names).
# With REPUBLISH=1, CRATES is not needed: the site is rebuilt from state under
# the current renderer and synced without publishing any crate.
# Optional env (defaults shown):
#   STUFF_DIR=../stuff REGISTRY=mycrates REG_ENV=MYCRATES
#   PUBLIC_URL=https://crates.17166969.xyz BUCKET=stuff-mycrates
#   STUFF_STATE_HOME=${XDG_STATE_HOME:-$HOME/.local/state}/stuff
#   STATE_DIR=$STUFF_STATE_HOME/mycrates/state
#   SITE_DIR=$STUFF_STATE_HOME/mycrates/site
#   TOKEN_FILE=$STUFF_STATE_HOME/mycrates/token
#   LOCK_DIR=$STUFF_STATE_HOME/mycrates/publish.lock
#   PUBLISH_PORT=8080
#
# State lives outside the calling checkout and the whole transaction is
# serialized, so publishing from two source repositories extends one index
# instead of replacing it with the caller's local subset.
set -eu
STUFF_DIR=${STUFF_DIR:-../stuff}
REGISTRY=${REGISTRY:-mycrates}
REG_ENV=${REG_ENV:-MYCRATES}
PUBLIC_URL=${PUBLIC_URL:-https://crates.17166969.xyz}
BUCKET=${BUCKET:-stuff-mycrates}
STUFF_STATE_HOME=${STUFF_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/stuff}
STATE_DIR=${STATE_DIR:-$STUFF_STATE_HOME/mycrates/state}
SITE_DIR=${SITE_DIR:-$STUFF_STATE_HOME/mycrates/site}
TOKEN_FILE=${TOKEN_FILE:-$STUFF_STATE_HOME/mycrates/token}
LOCK_DIR=${LOCK_DIR:-$STUFF_STATE_HOME/mycrates/publish.lock}
PUBLISH_PORT=${PUBLISH_PORT:-8080}
CALLER_MANIFEST=${CALLER_MANIFEST:-$PWD/Cargo.toml}
CALLER_TOOLCHAIN=
if command -v rustup >/dev/null 2>&1; then
  CALLER_TOOLCHAIN=$(rustup show active-toolchain 2>/dev/null | awk '{ print $1 }') || CALLER_TOOLCHAIN=
fi

. "$STUFF_DIR/deploy/publish-lock.sh"
LEGACY_STATE_DIR=${LEGACY_STATE_DIR:-$PWD/.work/registry-pub/state}
if { [ ! -d "$STATE_DIR" ] || [ -z "$(ls -A "$STATE_DIR" 2>/dev/null)" ]; } && [ -d "$LEGACY_STATE_DIR" ] && [ "$STATE_DIR" != "$LEGACY_STATE_DIR" ]; then
  echo "publish-mycrates: legacy per-checkout state exists at $LEGACY_STATE_DIR" >&2
  echo "publish-mycrates: move it to the canonical $STATE_DIR (or set STATE_DIR explicitly) so another repository cannot hide its packages" >&2
  exit 1
fi
mkdir -p "$STUFF_STATE_HOME/mycrates"
acquire_publish_lock "$LOCK_DIR"
SRV_PID=""
SITE_RESTORED=""
MYCRATES_BIN=$STUFF_DIR/crates/mycrates/bin/mycrates
R2_SYNC=$STUFF_DIR/deploy/r2-sync.sh
PRUNE=$STUFF_DIR/deploy/prune-generations.sh
LOCAL_API=http://127.0.0.1:$PUBLISH_PORT

restore_public_site() {
  "$MYCRATES_BIN" config set-public-base-url "$PUBLIC_URL" --state-dir "$STATE_DIR"
  "$MYCRATES_BIN" publish --state-dir "$STATE_DIR" --site-dir "$SITE_DIR"
  "$PRUNE" "$SITE_DIR"
}

cleanup() {
  status=$?
  trap - EXIT
  if [ -n "$SRV_PID" ]; then
    kill "$SRV_PID" 2>/dev/null || true
    wait "$SRV_PID" 2>/dev/null || true
  fi
  SRV_PID=""
  if [ -z "$SITE_RESTORED" ] && [ -x "$MYCRATES_BIN" ] && [ -d "$STATE_DIR" ]; then
    if restore_public_site; then
      echo "publish-mycrates: restored $PUBLIC_URL and rebuilt $SITE_DIR after failure" >&2
    else
      echo "publish-mycrates: WARNING: could not restore $PUBLIC_URL in $STATE_DIR" >&2
      echo "publish-mycrates: WARNING: do not sync $SITE_DIR until the public base URL is restored" >&2
    fi
  fi
  release_publish_lock || true
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

if [ -n "${REPUBLISH:-}" ]; then
  restore_public_site
  SITE_RESTORED=1
  "$R2_SYNC" "$SITE_DIR" "$BUCKET"
  exit 0
fi

[ -n "${CRATES:-}" ] || { echo "publish-mycrates: CRATES is empty" >&2; exit 1; }

mkdir -p "$STATE_DIR" "$SITE_DIR"
"$MYCRATES_BIN" init --state-dir "$STATE_DIR" --site-dir "$SITE_DIR" >/dev/null
"$MYCRATES_BIN" config set-public-base-url "$LOCAL_API" --state-dir "$STATE_DIR"
"$MYCRATES_BIN" publish --state-dir "$STATE_DIR" --site-dir "$SITE_DIR"
if command -v openssl >/dev/null 2>&1; then
  openssl rand -hex 32 >"$TOKEN_FILE"
else
  od -An -tx1 -N32 /dev/urandom | tr -d ' \n' >"$TOKEN_FILE"
fi
chmod 600 "$TOKEN_FILE"
"$MYCRATES_BIN" serve-publish --bind "127.0.0.1:$PUBLISH_PORT" --token-file "$TOKEN_FILE" \
  --state-dir "$STATE_DIR" --site-dir "$SITE_DIR" >/dev/null 2>&1 &
SRV_PID=$!
i=0
while ! curl -sf "$LOCAL_API/current/registry/config.json" >/dev/null; do
  i=$((i + 1))
  [ "$i" -lt 60 ] || { echo "publish-mycrates: local registry never came up" >&2; exit 1; }
  sleep 1
done
export "CARGO_REGISTRIES_${REG_ENV}_INDEX"="sparse+$LOCAL_API/current/registry/"
export "CARGO_REGISTRIES_${REG_ENV}_TOKEN"="$(cat "$TOKEN_FILE")"
for c in $CRATES; do
  if output=$(
    cd / || exit 1
    if [ -n "$CALLER_TOOLCHAIN" ]; then
      RUSTUP_TOOLCHAIN=$CALLER_TOOLCHAIN
      export RUSTUP_TOOLCHAIN
    fi
    cargo publish --manifest-path "$CALLER_MANIFEST" -p "$c" --registry "$REGISTRY" 2>&1
  ); then
    printf '%s\n' "$output"
  else
    status=$?
    if printf '%s\n' "$output" | grep -q "already exists on .*$REGISTRY.* index"; then
      echo "publish-mycrates: $c already published on $REGISTRY, skipping"
    else
      printf '%s\n' "$output" >&2
      exit "$status"
    fi
  fi
  "$MYCRATES_BIN" publish --state-dir "$STATE_DIR" --site-dir "$SITE_DIR"
done
kill "$SRV_PID" 2>/dev/null
wait "$SRV_PID" 2>/dev/null || true
SRV_PID=""
restore_public_site
SITE_RESTORED=1
"$R2_SYNC" "$SITE_DIR" "$BUCKET"
