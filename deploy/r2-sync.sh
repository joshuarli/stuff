#!/bin/sh
# Sync one published site directory to an R2 bucket, incrementally, then
# promote the generation atomically.
# Usage: r2-sync.sh SITE_DIR BUCKET_NAME [--expected ACTIVE_GEN] [--repair] [--dry-run]
#        [--promote=cf|s3]
# Env: CF_LOCAL_STATE sets a local simulation dir (appends --local).
#      R2_PARENT_ACCESS_KEY_ID, R2_ACCOUNT_ID, and R2_TEMP_CREDENTIALS_TTL
#      tune the temporary credentials minted when R2_S3_* is not set.
#
# Incremental and content-aware: lists remote objects through the S3 API when
# R2_S3_* is set, or by prefix through the cf CLI otherwise (deploy/
# r2-plan.py), and skips uploads whose remote size and content hash match the
# local file, so re-syncing an unchanged generation uploads nothing. Large
# files are compared by their multipart ETag. An existing immutable object
# with different bytes is a conflict; --repair is the explicit, separate way
# to overwrite it. The one exception is the manifest of a generation other
# than the one being promoted: it lists the shared store as it was when that
# generation was published, so the remote copy is kept.
#
# Uploads use the S3 API with R2_PUT_JOBS parallel requests (default 8) when
# R2_S3_* is set, or with short-lived bucket-scoped credentials minted from
# the authenticated cf CLI otherwise. If minting fails, uploads run through
# the cf CLI with one shared pacer at roughly four requests a second.
# CF_LOCAL_STATE keeps the single-threaded local simulation.
#
# Every sync writes manifests/<id>.json for each local generation and uploads
# it after every other object, so a generation directory without a manifest
# is always a sync in flight (r2-gc.py relies on this). It then promotes with
# deploy/r2-promote.py: the manifest's objects are verified
# remotely first, and `current` is replaced with If-Match (If-None-Match:*
# on first publish). A competing publisher gets 412 instead of overwriting
# the winner. Without --expected, the generation this machine last promoted
# (promoted-generation next to the registry's publish lock) is expected, so a
# site built from state another publisher has since superseded is refused.
# Roll back with deploy/r2-rollback.sh.
#
# --dry-run lists the bucket (read-only), prints how many objects and bytes
# would be uploaded, how many are skipped, and any conflicts, then exits
# without uploading or promoting. It still writes manifests/ into SITE_DIR and
# needs no R2_S3_* variables.
#
# Files over the 300 MB single-PUT cap (R2_SINGLE_PUT_MAX overrides) are uploaded via S3 multipart
# (deploy/r2-multipart.py, stdlib-only python3) using R2_S3_ENDPOINT,
# R2_S3_ACCESS_KEY_ID and R2_S3_SECRET_ACCESS_KEY. Promotion defaults to
# `cf` when the S3 variables are not all set, or when --promote=cf is passed:
# the authenticated `cf` CLI writes `current` with a plain put after the same
# remote verification. A competing publisher is not detected in that mode.
# --promote=s3 requires the S3 variables and uses a conditional write
# (If-Match, or If-None-Match:* on first publish); a competing publisher gets
# 412. Files over the single-PUT cap still need the S3 variables in either
# mode.
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
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
EXPECTED=""
REPAIR=""
DRY_RUN=""
PROMOTE=""
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
    --promote=cf|--promote=s3)
      PROMOTE=${1#--promote=}
      shift
      ;;
    *)
      echo "FAIL: usage: r2-sync.sh SITE_DIR BUCKET_NAME [--expected ACTIVE_GEN] [--repair] [--dry-run] [--promote=cf|s3]"
      exit 1
      ;;
  esac
done
s3_credentials_set() {
  [ -n "${R2_S3_ENDPOINT:-}" ] && [ -n "${R2_S3_ACCESS_KEY_ID:-}" ] && [ -n "${R2_S3_SECRET_ACCESS_KEY:-}" ]
}
if [ -z "${CF_LOCAL_STATE:-}" ] && [ -z "$DRY_RUN" ] && ! s3_credentials_set; then
  if credentials=$(python3 "$SCRIPT_DIR/r2-temp-credentials.py" "$BUCKET" 2>&1); then
    eval "$credentials"
  else
    echo "note: could not mint temporary R2 S3 credentials; using cf uploads" >&2
    printf '%s\n' "$credentials" | sed 's/^/note: /' >&2
  fi
fi
if [ -n "${CF_LOCAL_STATE:-}" ]; then
  mkdir -p "$CF_LOCAL_STATE"
elif [ -z "$DRY_RUN" ]; then
  if [ -z "$PROMOTE" ]; then
    if [ -n "${R2_PROMOTE_UNCONDITIONAL:-}" ]; then
      PROMOTE=cf
    else
      PROMOTE=s3
      for var in R2_S3_ENDPOINT R2_S3_ACCESS_KEY_ID R2_S3_SECRET_ACCESS_KEY; do
        eval "value=\${$var:-}"
        [ -n "$value" ] || { PROMOTE=cf; break; }
      done
    fi
  fi
  case $PROMOTE in
    cf)
      export R2_PROMOTE_UNCONDITIONAL=1
      echo "note: promoting through cf without a conditional write; a competing publisher would not be detected"
      ;;
    s3)
      unset R2_PROMOTE_UNCONDITIONAL
      for var in R2_S3_ENDPOINT R2_S3_ACCESS_KEY_ID R2_S3_SECRET_ACCESS_KEY; do
        eval "value=\${$var:-}"
        [ -n "$value" ] || { echo "FAIL: $var is not set; --promote=s3 needs the R2 S3 API"; exit 1; }
      done
      ;;
  esac
fi
if [ -n "$REPAIR" ]; then
  export R2_REPAIR=1
  echo "repair: existing immutable objects may be overwritten"
fi

. "$SCRIPT_DIR/publish-lock.sh"
RECORD=$(promoted_generation_path "$BUCKET")
if [ -z "$EXPECTED" ] && [ -f "$RECORD" ]; then
  EXPECTED=$(cat "$RECORD")
  echo "note: expecting remote current $EXPECTED, the generation this machine last promoted ($RECORD)"
fi

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

is_shared_store_path() {
  while IFS= read -r pattern; do
    case "$pattern" in
      "" | "#"*) continue ;;
    esac
    case "$1" in
      $pattern) return 0 ;;
    esac
  done < "$SCRIPT_DIR/shared-store-paths.txt"
  return 1
}

LIST=$(mktemp)
MANIFEST_KEYS=$(mktemp)
PLAN=$(mktemp)
PUTS=$(mktemp)
MANIFEST_PUTS=$(mktemp)
trap 'rm -f "$LIST" "$MANIFEST_KEYS" "$PLAN" "$PUTS" "$MANIFEST_PUTS"' EXIT
find "$SITE_DIR" -type f ! -path "$SITE_DIR/.*" | sort | while IFS= read -r f; do
  key=${f#"$SITE_DIR"/}
  if is_shared_store_path "$key"; then
    echo "skip alias $key" >&2
  else
    case $key in
      generations/*/stuff-manifest.json) ;;
      manifests/*.json) ;;
      *) printf '%s\n' "$key" ;;
    esac
  fi
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
awk -v active="conflict manifests/$GEN_ID.json" \
  '/^conflict manifests\// && $0 != active { sub(/^conflict /, "skip ") } { print }' "$PLAN" > "$PLAN.kept"
mv "$PLAN.kept" "$PLAN"
CONFLICTS=$(grep -c '^conflict ' "$PLAN" || true)
CONFLICTS=${CONFLICTS:-0}
if [ "$CONFLICTS" -gt 0 ]; then
  grep '^conflict ' "$PLAN" | sed 's/^conflict /FAIL: immutable object differs: /' >&2
  echo "FAIL: refusing to replace immutable content; run with --repair only after verifying the local generation" >&2
  exit 1
fi
SKIPPED=$(grep -c '^skip ' "$PLAN" || true)
SKIPPED=${SKIPPED:-0}
sed -n 's/^put //p' "$PLAN" | grep -v '^manifests/' > "$PUTS" || true
sed -n 's/^put //p' "$PLAN" | grep '^manifests/' > "$MANIFEST_PUTS" || true
UPLOADED=$(cat "$PUTS" "$MANIFEST_PUTS" | wc -l | tr -d ' ')
if [ -n "$DRY_RUN" ]; then
  PUT_BYTES=0
  while IFS= read -r key; do
    [ -n "$key" ] || continue
    PUT_BYTES=$((PUT_BYTES + $(wc -c < "$SITE_DIR/$key")))
  done <<PUT_KEYS
$(cat "$PUTS" "$MANIFEST_PUTS")
PUT_KEYS
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
python3 "$SCRIPT_DIR/r2-put.py" "$SITE_DIR" "$BUCKET" "$PUTS"
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
  python3 "$SCRIPT_DIR/r2-multipart.py" "$SITE_DIR/$key" "$BUCKET" "$key" "$(python3 "$SCRIPT_DIR/r2-put.py" --type "$key")"
  UPLOADED=$((UPLOADED + 1))
done <<MULTI
$(grep '^multipart ' "$PLAN" || true)
MULTI
if [ -s "$MANIFEST_PUTS" ]; then
  python3 "$SCRIPT_DIR/r2-put.py" "$SITE_DIR" "$BUCKET" "$MANIFEST_PUTS"
fi

echo "activate $GEN_ID (uploaded $UPLOADED, skipped $SKIPPED)"
if [ -n "$EXPECTED" ]; then
  python3 "$SCRIPT_DIR/r2-promote.py" "$BUCKET" "$GEN_ID" --expected "$EXPECTED"
else
  python3 "$SCRIPT_DIR/r2-promote.py" "$BUCKET" "$GEN_ID"
fi
record_promoted_generation "$BUCKET" "$GEN_ID"
