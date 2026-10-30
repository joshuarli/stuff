#!/bin/sh
# Publish crates to our private mycrates registry and sync the site to R2.
# Called from the source repos' Makefiles (smolith, h12tiny), which own their
# crate lists. Runs the whole flow in one shell under `set -eu` with server
# cleanup on exit, so a failed `cargo publish` aborts before any R2 write.
#
# Required env: CRATES="name-a name-b ..." (space-separated package names).
# Optional env (defaults shown):
#   STUFF_DIR=../stuff REGISTRY=mycrates REG_ENV=MYCRATES
#   PUBLIC_URL=https://crates.17166969.xyz BUCKET=stuff-mycrates
#   WORK_DIR=.work/registry-pub PUBLISH_PORT=8080 STATIC_PORT=8081
set -eu
STUFF_DIR=${STUFF_DIR:-../stuff}
REGISTRY=${REGISTRY:-mycrates}
REG_ENV=${REG_ENV:-MYCRATES}
PUBLIC_URL=${PUBLIC_URL:-https://crates.17166969.xyz}
BUCKET=${BUCKET:-stuff-mycrates}
WORK_DIR=${WORK_DIR:-.work/registry-pub}
STATE_DIR=${STATE_DIR:-$WORK_DIR/state}
SITE_DIR=${SITE_DIR:-$WORK_DIR/site}
TOKEN_FILE=${TOKEN_FILE:-$WORK_DIR/token}
PUBLISH_PORT=${PUBLISH_PORT:-8080}
STATIC_PORT=${STATIC_PORT:-8081}

MYCRATES_BIN=$STUFF_DIR/target/debug/mycrates
SERVER_BIN=$STUFF_DIR/target/debug/stuff-server
R2_SYNC=$STUFF_DIR/deploy/r2-sync.sh
LOCAL_API=http://127.0.0.1:$PUBLISH_PORT
LOCAL_SITE=http://127.0.0.1:$STATIC_PORT

[ -n "${CRATES:-}" ] || { echo "publish-mycrates: CRATES is empty" >&2; exit 1; }

cargo build -p mycrates -p stuff-server --manifest-path "$STUFF_DIR/Cargo.toml"
mkdir -p "$STATE_DIR" "$SITE_DIR"
"$MYCRATES_BIN" init --state-dir "$STATE_DIR" --site-dir "$SITE_DIR" >/dev/null
python3 -c "import json; p='$STATE_DIR/config.json'; c=json.load(open(p)); c['public_base_url']='$LOCAL_API'; json.dump(c, open(p, 'w'), indent=2)"
"$MYCRATES_BIN" publish --state-dir "$STATE_DIR" --site-dir "$SITE_DIR"
python3 -c "import secrets; open('$TOKEN_FILE', 'w').write(secrets.token_hex(32))"
chmod 600 "$TOKEN_FILE"
"$MYCRATES_BIN" serve-publish --bind "127.0.0.1:$PUBLISH_PORT" --token-file "$TOKEN_FILE" \
  --state-dir "$STATE_DIR" --site-dir "$SITE_DIR" >/dev/null 2>&1 &
SRV_PID=$!
"$SERVER_BIN" --bind "127.0.0.1:$STATIC_PORT" --route "/=$SITE_DIR" >/dev/null 2>&1 &
STATIC_PID=$!
trap 'kill $SRV_PID $STATIC_PID 2>/dev/null' EXIT
i=0
while ! curl -sf "$LOCAL_SITE/current/registry/config.json" >/dev/null; do
  i=$((i + 1))
  [ "$i" -lt 60 ] || { echo "publish-mycrates: static server never came up" >&2; exit 1; }
  sleep 1
done
export "CARGO_REGISTRIES_${REG_ENV}_INDEX"="sparse+$LOCAL_SITE/current/registry/"
export "CARGO_REGISTRIES_${REG_ENV}_TOKEN"="$(cat "$TOKEN_FILE")"
for c in $CRATES; do
  cargo publish -p "$c" --registry "$REGISTRY"
  "$MYCRATES_BIN" publish --state-dir "$STATE_DIR" --site-dir "$SITE_DIR"
done
kill $SRV_PID $STATIC_PID 2>/dev/null
trap - EXIT
python3 -c "import json; p='$STATE_DIR/config.json'; c=json.load(open(p)); c['public_base_url']='$PUBLIC_URL'; json.dump(c, open(p, 'w'), indent=2)"
rm -rf "$SITE_DIR"
mkdir -p "$SITE_DIR"
"$MYCRATES_BIN" publish --state-dir "$STATE_DIR" --site-dir "$SITE_DIR"
"$R2_SYNC" "$SITE_DIR" "$BUCKET"
