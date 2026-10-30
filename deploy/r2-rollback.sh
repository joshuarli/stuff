#!/bin/sh
# Roll the R2 `current` marker back to an older local generation.
# Usage: r2-rollback.sh SITE_DIR BUCKET_NAME GENERATION_ID
# Env: CF_LOCAL_STATE sets a local simulation dir (appends --local).
#
# The target generation must exist locally with a catalog.json (i.e. it was
# produced by the current publishers). After moving the marker, the value is
# read back and verified. Rolling forward again is the same command with the
# newer generation id.
set -eu
SITE_DIR=$1
BUCKET=$2
GEN_ID=$3
LOCAL_ARGS=""
if [ -n "${CF_LOCAL_STATE:-}" ]; then
  mkdir -p "$CF_LOCAL_STATE"
  LOCAL_ARGS="--local --persist-to $CF_LOCAL_STATE"
fi
case $GEN_ID in
  *[!A-Za-z0-9_-]* | "") echo "FAIL: bad generation id: $GEN_ID"; exit 1 ;;
esac
GEN_DIR="$SITE_DIR/generations/$GEN_ID"
[ -d "$GEN_DIR" ] || { echo "FAIL: local generation missing: $GEN_DIR"; exit 1; }
[ -f "$GEN_DIR/catalog.json" ] || { echo "FAIL: local generation has no catalog.json: $GEN_DIR"; exit 1; }

MARKER=$(mktemp)
trap 'rm -f "$MARKER"' EXIT
printf '%s' "$GEN_ID" > "$MARKER"
# shellcheck disable=SC2086
cf r2 objects put current --bucket-name "$BUCKET" --file "$MARKER" \
  --content-type "text/plain" $LOCAL_ARGS >/dev/null
echo "rolled back to $GEN_ID"

CHECK=$(mktemp)
trap 'rm -f "$MARKER" "$CHECK"' EXIT
# shellcheck disable=SC2086
cf r2 objects get current --bucket-name "$BUCKET" --text $LOCAL_ARGS >"$CHECK" 2>/dev/null
id=$(tr -d '[:space:]' <"$CHECK")
[ "$id" = "$GEN_ID" ] || { echo "FAIL: remote current is '${id:-<missing>}' after rollback, expected '$GEN_ID'"; exit 1; }
echo "verified current is $GEN_ID"
