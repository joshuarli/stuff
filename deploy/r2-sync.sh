#!/bin/sh
# Sync one published site directory to an R2 bucket, incrementally, then
# promote the generation atomically.
# Usage: r2-sync.sh SITE_DIR BUCKET_NAME [--expected ACTIVE_GEN] [--repair] [--dry-run]
# Env: CF_LOCAL_STATE sets a local simulation dir (appends --local).
#
# Incremental and content-aware: lists remote objects by prefix (deploy/
# r2-plan.py, no pagination cursors, any object count) and skips uploads whose
# remote size and content hash match the local file, so re-syncing an
# unchanged generation uploads nothing. Large files are compared by their
# multipart ETag. An existing immutable object with different bytes is a
# conflict; --repair is the explicit, separate way to overwrite it.
#
# Every sync writes manifests/<id>.json for each local generation, then
# promotes with deploy/r2-promote.py: the manifest's objects are verified
# remotely first, and `current` is replaced with If-Match (If-None-Match:*
# on first publish). A competing publisher gets 412 instead of overwriting
# the winner. Roll back with deploy/r2-rollback.sh.
#
# --dry-run lists the bucket (read-only), prints how many objects and bytes
# would be uploaded, how many are skipped, and any conflicts, then exits
# without uploading or promoting. It still writes manifests/ into SITE_DIR and
# needs no R2_S3_* variables.
#
# Files over the 300 MB single-PUT cap (R2_SINGLE_PUT_MAX overrides) are uploaded via S3 multipart
# (deploy/r2-multipart.py, stdlib-only python3) using R2_S3_ENDPOINT,
# R2_S3_ACCESS_KEY_ID and R2_S3_SECRET_ACCESS_KEY. Conditional promotion
# needs those variables even when no file is large. R2_PROMOTE_UNCONDITIONAL=1
# instead promotes through the authenticated `cf` CLI with a plain put of
# `current` (after the same remote verification): no S3 credentials are needed,
# but a competing publisher is not detected. Files over the single-PUT cap
# still need the S3 variables.
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
shift 2
EXPECTED=""
REPAIR=""
DRY_RUN=""
while [ $# -gt 0 ]; do
  case $1 in
    --expected)
      [ $# -ge 2 ] && [ -n "$2" ] || { echo "FAIL: --expected requires a generation id"; exit 1; }
      EXPECTED=$2
      shift 2
      ;;
    --repair)
      REPAIR=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    *)
      echo "FAIL: usage: r2-sync.sh SITE_DIR BUCKET_NAME [--expected ACTIVE_GEN] [--repair] [--dry-run]"
      exit 1
      ;;
  esac
done
if [ -n "${CF_LOCAL_STATE:-}" ]; then
  mkdir -p "$CF_LOCAL_STATE"
elif [ -n "${R2_PROMOTE_UNCONDITIONAL:-}" ]; then
  echo "note: promoting through cf without a conditional write; a competing publisher would not be detected"
elif [ -z "$DRY_RUN" ]; then
  for var in R2_S3_ENDPOINT R2_S3_ACCESS_KEY_ID R2_S3_SECRET_ACCESS_KEY; do
    eval "value=\${$var:-}"
    [ -n "$value" ] || { echo "FAIL: $var is not set; conditional promotion needs the R2 S3 API"; exit 1; }
  done
fi
if [ -n "$REPAIR" ]; then
  export R2_REPAIR=1
  echo "repair: existing immutable objects may be overwritten"
fi
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)

[ -L "$SITE_DIR/current" ] || { echo "FAIL: $SITE_DIR/current is not a symlink"; exit 1; }
GEN_TARGET=$(readlink "$SITE_DIR/current")
GEN_ID=${GEN_TARGET#generations/}
case $GEN_ID in
  *[!A-Za-z0-9_-]* | "") echo "FAIL: bad generation id: $GEN_ID"; exit 1 ;;
esac

echo "note: generation alias paths resolve server-side (see worker alias_key); skipping their upload"
find "$SITE_DIR" -type l ! -path "$SITE_DIR/.*" | sort | while IFS= read -r f; do
  echo "note: skipping symlink ${f#"$SITE_DIR"/}"
done

LIST=$(mktemp)
MANIFEST_KEYS=$(mktemp)
PLAN=$(mktemp)
PUTS=$(mktemp)
trap 'rm -f "$LIST" "$MANIFEST_KEYS" "$PLAN" "$PUTS"' EXIT
find "$SITE_DIR" -type f ! -path "$SITE_DIR/.*" | sort | while IFS= read -r f; do
  key=${f#"$SITE_DIR"/}
  case $key in
    generations/*/tarballs/*) echo "skip alias $key" >&2 ;;
    generations/*/stuff-manifest.json) ;;
    manifests/*.json) ;;
    *) printf '%s\n' "$key" ;;
  esac
done > "$LIST"

mkdir -p "$SITE_DIR/manifests"
for gen_dir in "$SITE_DIR"/generations/*; do
  [ -d "$gen_dir" ] || continue
  gen=${gen_dir##*/}
  case $gen in
    "" | *[!A-Za-z0-9_-]*) continue ;;
  esac
  while IFS= read -r key; do
    case $key in
      generations/"$gen"/*) printf '%s\n' "$key" ;;
      generations/*) ;;
      manifests/*) ;;
      *) printf '%s\n' "$key" ;;
    esac
  done < "$LIST" > "$MANIFEST_KEYS"
  python3 "$SCRIPT_DIR/r2-manifest.py" "$SITE_DIR" "$SITE_DIR/manifests/$gen.json" "$MANIFEST_KEYS"
  printf 'manifests/%s.json\n' "$gen" >> "$LIST"
done

if [ -s "$LIST" ]; then
  python3 "$SCRIPT_DIR/r2-plan.py" "$SITE_DIR" "$BUCKET" "$LIST" > "$PLAN"
fi
CONFLICTS=$(grep -c '^conflict ' "$PLAN" || true)
CONFLICTS=${CONFLICTS:-0}
if [ "$CONFLICTS" -gt 0 ]; then
  grep '^conflict ' "$PLAN" | sed 's/^conflict /FAIL: immutable object differs: /' >&2
  echo "FAIL: refusing to replace immutable content; run with --repair only after verifying the local generation" >&2
  exit 1
fi
SKIPPED=$(grep -c '^skip ' "$PLAN" || true)
SKIPPED=${SKIPPED:-0}
grep '^skip ' "$PLAN" | sed 's/^skip /skip unchanged /' || true
sed -n 's/^put //p' "$PLAN" > "$PUTS"
UPLOADED=$(wc -l < "$PUTS" | tr -d ' ')
if [ -n "$DRY_RUN" ]; then
  PUT_BYTES=0
  while IFS= read -r key; do
    [ -n "$key" ] || continue
    PUT_BYTES=$((PUT_BYTES + $(wc -c < "$SITE_DIR/$key")))
  done < "$PUTS"
  MULTIPART=$(grep -c '^multipart ' "$PLAN" || true)
  MULTIPART_BYTES=0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    MULTIPART_BYTES=$((MULTIPART_BYTES + ${line##* }))
  done <<MULTI_SIZES
$(grep '^multipart ' "$PLAN" || true)
MULTI_SIZES
  echo "dry-run $GEN_ID: put $UPLOADED ($PUT_BYTES bytes), multipart ${MULTIPART:-0} ($MULTIPART_BYTES bytes), skip $SKIPPED, conflict 0"
  exit 0
fi
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

echo "activate $GEN_ID (uploaded $UPLOADED, skipped $SKIPPED)"
if [ -n "$EXPECTED" ]; then
  python3 "$SCRIPT_DIR/r2-promote.py" "$BUCKET" "$GEN_ID" --expected "$EXPECTED"
else
  python3 "$SCRIPT_DIR/r2-promote.py" "$BUCKET" "$GEN_ID"
fi
