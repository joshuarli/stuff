#!/bin/sh
# Sync one published site directory to an R2 bucket, incrementally.
# Usage: r2-sync.sh SITE_DIR BUCKET_NAME [--expected GEN_ID]
# Env: CF_LOCAL_STATE sets a local simulation dir (appends --local).
#
# Incremental and content-aware: lists remote objects once and skips uploads
# whose remote size and MD5 match the local file, so re-syncing an unchanged
# generation uploads nothing. Generation-scoped immutable aliases are not
# uploaded (the worker resolves them to the shared store); content addressing
# still holds because skipped objects are verified by size and hash, not by
# key existence alone.
#
# Promotion is guarded: --expected aborts unless the remote `current` marker
# already holds that generation id, so competing publishers cannot silently
# overwrite each other. After uploading, the new marker is read back and
# verified. Roll back with deploy/r2-rollback.sh.
#
# Files over the 300 MB single-PUT cap are uploaded via S3 multipart
# (deploy/r2-multipart.py, stdlib-only python3) using R2_S3_ENDPOINT,
# R2_S3_ACCESS_KEY_ID and R2_S3_SECRET_ACCESS_KEY; without those, such files
# fail loudly instead of being skipped.
#
# Generation-scoped immutable aliases are NOT uploaded: the worker resolves
# them to the shared store (see stuff-worker alias_key).
# Skipped: generations/*/tarballs/*, generations/*/-/tarballs/* (mynpm
# hardlink aliases of the root tarballs/ store) and symlinked directories
# such as generations/*/wheels (mypypi link to the shared wheels/ store;
# find does not descend into them).
set -eu
SITE_DIR=$1
BUCKET=$2
EXPECTED=""
if [ "${3:-}" = "--expected" ]; then
  EXPECTED=${4:-}
  [ -n "$EXPECTED" ] || { echo "FAIL: --expected requires a generation id"; exit 1; }
elif [ -n "${3:-}" ]; then
  echo "FAIL: usage: r2-sync.sh SITE_DIR BUCKET_NAME [--expected GEN_ID]"
  exit 1
fi
LOCAL_ARGS=""
if [ -n "${CF_LOCAL_STATE:-}" ]; then
  mkdir -p "$CF_LOCAL_STATE"
  LOCAL_ARGS="--local --persist-to $CF_LOCAL_STATE"
fi
SINGLE_PUT_MAX=314572800
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)

mime_for() {
  case $1 in
    *.json) printf 'application/json' ;;
    *.html) printf 'text/html; charset=utf-8' ;;
    *.txt | *.md) printf 'text/markdown; charset=utf-8' ;;
    *.toml) printf 'text/plain; charset=utf-8' ;;
    *.crate) printf 'application/x-tar' ;;
    *.tgz | *.whl | *.gz) printf 'application/octet-stream' ;;
    *.metadata) printf 'text/plain; charset=utf-8' ;;
    *) printf 'application/octet-stream' ;;
  esac
}

md5_of() {
  if command -v md5sum >/dev/null 2>&1; then
    md5sum "$1" | cut -d' ' -f1
  else
    md5 -q "$1"
  fi
}

# shellcheck disable=SC2086
put_file() {
  cf r2 objects put "$2" --bucket-name "$BUCKET" --file "$1" \
    --content-type "$3" $LOCAL_ARGS >/dev/null
}

# shellcheck disable=SC2086
remote_current() {
  tmp=$(mktemp)
  if cf r2 objects get current --bucket-name "$BUCKET" --text $LOCAL_ARGS >"$tmp" 2>/dev/null; then
    id=$(tr -d '[:space:]' <"$tmp")
    rm -f "$tmp"
    case $id in
      *[!A-Za-z0-9_-]* | "") return 1 ;;
      *) printf '%s' "$id"; return 0 ;;
    esac
  else
    rm -f "$tmp"
    return 1
  fi
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

if [ -n "$EXPECTED" ]; then
  REMOTE=$(remote_current || true)
  [ "$REMOTE" = "$EXPECTED" ] || { echo "FAIL: remote current is '${REMOTE:-<missing>}', expected '$EXPECTED'; refusing to overwrite a competing publication"; exit 1; }
  echo "guard: remote current is $EXPECTED as expected"
else
  REMOTE=$(remote_current || true)
  if [ -n "$REMOTE" ] && [ "$REMOTE" != "$GEN_ID" ]; then
    echo "note: remote current is $REMOTE; activating $GEN_ID"
  fi
fi

echo "note: generation alias paths resolve server-side (see worker alias_key); skipping their upload"
find "$SITE_DIR" -type l ! -path "$SITE_DIR/.*" | sort | while IFS= read -r f; do
  echo "note: skipping symlink ${f#$SITE_DIR/}"
done

MARKER=$(mktemp)
LIST=$(mktemp)
REMOTE_LIST=$(mktemp)
trap 'rm -f "$MARKER" "$LIST" "$REMOTE_LIST"' EXIT
find "$SITE_DIR" -type f ! -path "$SITE_DIR/.*" | sort > "$LIST"
# shellcheck disable=SC2086
cf r2 objects list --bucket-name "$BUCKET" --per-page 1000 $LOCAL_ARGS >"$REMOTE_LIST"
if [ "$(jaq 'length' "$REMOTE_LIST")" -ge 1000 ]; then
  echo "FAIL: remote object listing hit the page limit; pagination support needed"
  exit 1
fi
UPLOADED=0
SKIPPED=0
while IFS= read -r f; do
  key=${f#$SITE_DIR/}
  case $key in
    generations/*/tarballs/*) echo "skip alias $key"; continue ;;
  esac
  size=$(file_bytes "$f")
  if [ "$size" -gt "$SINGLE_PUT_MAX" ]; then
    if [ -n "${R2_S3_ENDPOINT:-}" ] && [ -n "${R2_S3_ACCESS_KEY_ID:-}" ] && [ -n "${R2_S3_SECRET_ACCESS_KEY:-}" ]; then
      echo "multipart $key ($size bytes)"
      python3 "$SCRIPT_DIR/r2-multipart.py" "$f" "$BUCKET" "$key" "$(mime_for "$key")"
      UPLOADED=$((UPLOADED + 1))
      continue
    fi
    echo "FAIL: $key ($size bytes) exceeds the 300 MB single-PUT cap and R2_S3_* multipart credentials are not set"
    exit 1
  fi
  want=$(jaq -r --arg k "$key" '.[] | select(.key == $k) | "\(.size) \(.etag)"' "$REMOTE_LIST" | head -n 1)
  if [ -n "$want" ]; then
    remote_size=${want%% *}
    remote_etag=${want#* }
    if [ "$remote_size" = "$size" ]; then
      local_md5=$(md5_of "$f")
      if [ "$remote_etag" = "$local_md5" ]; then
        echo "skip unchanged $key"
        SKIPPED=$((SKIPPED + 1))
        continue
      fi
    fi
  fi
  put_file "$f" "$key" "$(mime_for "$key")"
  echo "put $key"
  UPLOADED=$((UPLOADED + 1))
done < "$LIST"

printf '%s' "$GEN_ID" > "$MARKER"
put_file "$MARKER" "current" "text/plain"
echo "activate $GEN_ID (uploaded $UPLOADED, skipped $SKIPPED)"

VERIFY=$(remote_current || true)
[ "$VERIFY" = "$GEN_ID" ] || { echo "FAIL: remote current is '${VERIFY:-<missing>}' after activation, expected '$GEN_ID'"; exit 1; }
echo "verified current is $GEN_ID"
