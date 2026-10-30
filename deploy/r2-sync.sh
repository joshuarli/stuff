#!/bin/sh
# Sync one published site directory to an R2 bucket.
# Usage: r2-sync.sh SITE_DIR BUCKET_NAME
# Env: CF_LOCAL_STATE sets a local simulation dir (appends --local).
# Translates the `current` symlink into a marker object holding the
# generation id. Single-PUT cap is 300 MB per object (cf endpoint).
set -eu
SITE_DIR=$1
BUCKET=$2
LOCAL_ARGS=""
if [ -n "${CF_LOCAL_STATE:-}" ]; then
  mkdir -p "$CF_LOCAL_STATE"
  LOCAL_ARGS="--local --persist-to $CF_LOCAL_STATE"
fi

mime_for() {
  case $1 in
    *.json) printf 'application/json' ;;
    *.html) printf 'text/html; charset=utf-8' ;;
    *.toml) printf 'text/plain; charset=utf-8' ;;
    *.crate) printf 'application/x-tar' ;;
    *.tgz | *.whl | *.gz) printf 'application/octet-stream' ;;
    *.metadata) printf 'text/plain; charset=utf-8' ;;
    *) printf 'application/octet-stream' ;;
  esac
}

# shellcheck disable=SC2086
put_file() {
  cf r2 objects put "$2" --bucket-name "$BUCKET" --file "$1" \
    --content-type "$3" $LOCAL_ARGS >/dev/null
}

[ -L "$SITE_DIR/current" ] || { echo "FAIL: $SITE_DIR/current is not a symlink"; exit 1; }
GEN_TARGET=$(readlink "$SITE_DIR/current")
GEN_ID=${GEN_TARGET#generations/}
case $GEN_ID in
  *[!A-Za-z0-9_-]* | "") echo "FAIL: bad generation id: $GEN_ID"; exit 1 ;;
esac

find "$SITE_DIR" -type f | sort | while IFS= read -r f; do
  key=${f#$SITE_DIR/}
  put_file "$f" "$key" "$(mime_for "$key")"
  echo "put $key"
done

MARKER="$SITE_DIR/.current-marker"
printf '%s' "$GEN_ID" > "$MARKER"
put_file "$MARKER" "current" "text/plain"
rm -f "$MARKER"
echo "activate $GEN_ID"
