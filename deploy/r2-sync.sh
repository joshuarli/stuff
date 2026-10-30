#!/bin/sh
# Sync one published site directory to an R2 bucket.
# Usage: r2-sync.sh SITE_DIR BUCKET_NAME
# Env: CF_LOCAL_STATE sets a local simulation dir (appends --local).
# Translates the `current` symlink into a marker object holding the
# generation id. Single-PUT cap is 300 MB per object (cf endpoint);
# larger files fail with an explicit error (multipart upload is a
# manual followup, see docs).
#
# Generation-scoped immutable aliases are NOT uploaded: the worker
# resolves them to the shared store (see stuff-worker alias_key).
# Skipped: generations/*/tarballs/*, generations/*/-/tarballs/* (mynpm
# hardlink aliases of the root tarballs/ store) and symlinked
# directories such as generations/*/wheels (mypypi link to the shared
# wheels/ store; find does not descend into them).
set -eu
SITE_DIR=$1
BUCKET=$2
LOCAL_ARGS=""
if [ -n "${CF_LOCAL_STATE:-}" ]; then
  mkdir -p "$CF_LOCAL_STATE"
  LOCAL_ARGS="--local --persist-to $CF_LOCAL_STATE"
fi
SINGLE_PUT_MAX=314572800

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

file_bytes() {
  if stat -f%z "$1" >/dev/null 2>&1; then
    stat -f%z "$1"
  else
    stat -c%s "$1"
  fi
}

[ -L "$SITE_DIR/current" ] || { echo "FAIL: $SITE_DIR/current is not a symlink"; exit 1; }
GEN_TARGET=$(readlink "$SITE_DIR/current")
GEN_ID=${GEN_TARGET#generations/}
case $GEN_ID in
  *[!A-Za-z0-9_-]* | "") echo "FAIL: bad generation id: $GEN_ID"; exit 1 ;;
esac

echo "note: generation alias paths resolve server-side (see worker alias_key); skipping their upload"
find "$SITE_DIR" -type l ! -path '*/.*' | sort | while IFS= read -r f; do
  echo "note: skipping symlink ${f#$SITE_DIR/}"
done

MARKER=$(mktemp)
LIST=$(mktemp)
trap 'rm -f "$MARKER" "$LIST"' EXIT
find "$SITE_DIR" -type f ! -path '*/.*' | sort > "$LIST"
while IFS= read -r f; do
  key=${f#$SITE_DIR/}
  case $key in
    generations/*/tarballs/*) echo "skip alias $key"; continue ;;
  esac
  size=$(file_bytes "$f")
  if [ "$size" -gt "$SINGLE_PUT_MAX" ]; then
    echo "FAIL: $key ($size bytes) exceeds the 300 MB single-PUT cap; upload it via multipart out of band"
    exit 1
  fi
  put_file "$f" "$key" "$(mime_for "$key")"
  echo "put $key"
done < "$LIST"

printf '%s' "$GEN_ID" > "$MARKER"
put_file "$MARKER" "current" "text/plain"
echo "activate $GEN_ID"
