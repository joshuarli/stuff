#!/bin/sh
# Sync one published site directory to an R2 bucket, incrementally.
# Usage: r2-sync.sh SITE_DIR BUCKET_NAME [--expected GEN_ID]
# Env: CF_LOCAL_STATE sets a local simulation dir (appends --local).
#
# Incremental and content-aware: lists remote objects by prefix (deploy/
# r2-plan.py, no pagination cursors, any object count) and skips uploads whose
# remote size and content hash match the local file, so re-syncing an
# unchanged generation uploads nothing. Large files are compared by their
# multipart ETag. Generation-scoped immutable aliases are not
# uploaded (the worker resolves them to the shared store); content addressing
# still holds because skipped objects are verified by size and hash, not by
# key existence alone.
#
# Promotion is guarded: --expected aborts unless the remote `current` marker
# already holds that generation id, so competing publishers cannot silently
# overwrite each other. After uploading, the new marker is read back and
# verified. Roll back with deploy/r2-rollback.sh.
#
# Files over the 300 MB single-PUT cap (R2_SINGLE_PUT_MAX overrides) are uploaded via S3 multipart
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
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)

# shellcheck disable=SC2086
put_marker() {
  cf r2 objects put current --bucket-name "$BUCKET" --file "$1" \
    --content-type "text/plain" $LOCAL_ARGS >/dev/null
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
PLAN=$(mktemp)
PUTS=$(mktemp)
trap 'rm -f "$MARKER" "$LIST" "$PLAN" "$PUTS"' EXIT
find "$SITE_DIR" -type f ! -path "$SITE_DIR/.*" | sort | while IFS= read -r f; do
  key=${f#$SITE_DIR/}
  case $key in
    generations/*/tarballs/*) echo "skip alias $key" >&2 ;;
    *) printf '%s\n' "$key" ;;
  esac
done > "$LIST"
if [ -s "$LIST" ]; then
  python3 "$SCRIPT_DIR/r2-plan.py" "$SITE_DIR" "$BUCKET" "$LIST" > "$PLAN"
fi
SKIPPED=$(grep -c '^skip ' "$PLAN" || true)
SKIPPED=${SKIPPED:-0}
grep '^skip ' "$PLAN" | sed 's/^skip /skip unchanged /' || true
sed -n 's/^put //p' "$PLAN" > "$PUTS"
UPLOADED=$(wc -l < "$PUTS" | tr -d ' ')
export SITE_DIR BUCKET CF_LOCAL_STATE
if [ -n "${CF_LOCAL_STATE:-}" ]; then
  PUT_JOBS=${R2_PUT_JOBS:-1}
else
  PUT_JOBS=${R2_PUT_JOBS:-8}
fi
tr '\n' '\0' < "$PUTS" | xargs -0 -n 1 -P "$PUT_JOBS" "$SCRIPT_DIR/r2-put.sh"
while IFS= read -r line; do
  [ -n "$line" ] || continue
  rest=${line#multipart }
  size=${rest##* }
  key=${rest% *}
  if [ -z "${R2_S3_ENDPOINT:-}" ] || [ -z "${R2_S3_ACCESS_KEY_ID:-}" ] || [ -z "${R2_S3_SECRET_ACCESS_KEY:-}" ]; then
    echo "FAIL: $key ($size bytes) exceeds the 300 MB single-PUT cap and R2_S3_* multipart credentials are not set"
    exit 1
  fi
  echo "multipart $key ($size bytes)"
  python3 "$SCRIPT_DIR/r2-multipart.py" "$SITE_DIR/$key" "$BUCKET" "$key" "$("$SCRIPT_DIR/r2-put.sh" --type "$key")"
  UPLOADED=$((UPLOADED + 1))
done <<MULTI
$(grep '^multipart ' "$PLAN" || true)
MULTI

printf '%s' "$GEN_ID" > "$MARKER"
put_marker "$MARKER"
echo "activate $GEN_ID (uploaded $UPLOADED, skipped $SKIPPED)"

VERIFY=$(remote_current || true)
[ "$VERIFY" = "$GEN_ID" ] || { echo "FAIL: remote current is '${VERIFY:-<missing>}' after activation, expected '$GEN_ID'"; exit 1; }
echo "verified current is $GEN_ID"
