#!/bin/sh
# Upload the immutable OCI blob store before any metadata.
# Usage: r2-sync-blobs.sh SITE_DIR [BUCKET]
# Env: CF_LOCAL_STATE (local simulation), R2_S3_* for multipart blobs (minted
# for BUCKET from cf when unset).
set -eu
SITE_DIR=$1
BUCKET=${2:-stuff-oci-blobs}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TMPDIR=${TMPDIR:-"$ROOT/.work/tmp"}
export TMPDIR
mkdir -p "$TMPDIR"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}
[ -d "$SITE_DIR/blobs/sha256" ] || {
  echo "sync-blobs: no blobs in $SITE_DIR"
  exit 0
}

STAGE=$(mktemp -d "$TMPDIR/oci-blobs-sync.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT INT TERM
mkdir -p "$STAGE/sha256"
COUNT=0
for file in "$SITE_DIR"/blobs/sha256/*; do
  [ -f "$file" ] || continue
  hex=$(basename "$file")
  case $hex in
    *[!0-9a-f]*) fail "blob name is not lowercase hex: $hex" ;;
  esac
  [ "${#hex}" -eq 64 ] || fail "blob name is not a sha256 hex: $hex"
  ln "$file" "$STAGE/sha256/$hex" 2>/dev/null || cp "$file" "$STAGE/sha256/$hex"
  COUNT=$((COUNT + 1))
done
[ "$COUNT" -gt 0 ] || {
  echo "sync-blobs: no blobs in $SITE_DIR"
  exit 0
}

KEYS=$STAGE.keys
PLAN=$STAGE.plan
(cd "$STAGE" && find . -type f | sed 's#^\./##' | sort) > "$KEYS"
python3 "$ROOT/deploy/r2-plan.py" "$STAGE" "$BUCKET" "$KEYS" > "$PLAN"

if grep -q '^conflict ' "$PLAN"; then
  grep '^conflict ' "$PLAN" | sed 's/^conflict /FAIL: immutable blob differs: /' >&2
  fail "refusing to replace content-addressed blobs"
fi

PUTS=$STAGE.puts
MULTIPART=$STAGE.multipart
grep '^put ' "$PLAN" | awk '{print $2}' > "$PUTS" || true
grep '^multipart ' "$PLAN" > "$MULTIPART" || true
SKIPPED=$(grep -c '^skip ' "$PLAN" || true)
PUT_COUNT=$(grep -c . "$PUTS" || true)
MULTIPART_COUNT=$(grep -c . "$MULTIPART" || true)

if [ "$PUT_COUNT" -gt 0 ]; then
  python3 "$ROOT/deploy/r2-put.py" "$STAGE" "$BUCKET" "$PUTS"
fi
if [ "$MULTIPART_COUNT" -gt 0 ]; then
  if [ -z "${CF_LOCAL_STATE:-}" ]; then
    SCRIPT_DIR=$ROOT/deploy
    . "$SCRIPT_DIR/r2-s3-credentials.sh"
    mint_s3_credentials "$BUCKET" ||
      fail "large blobs need R2_S3_* and minting them failed; mint them with: eval \"\$(python3 $SCRIPT_DIR/r2-temp-credentials.py $BUCKET)\""
  fi
  for var in R2_S3_ENDPOINT R2_S3_ACCESS_KEY_ID R2_S3_SECRET_ACCESS_KEY; do
    eval "value=\${$var:-}"
    [ -n "$value" ] || fail "$var is not set; large blobs need the R2 S3 API"
  done
  while read -r _ key size; do
    [ -n "$key" ] || continue
    echo "multipart $key ($size bytes)"
    python3 "$ROOT/deploy/r2-multipart.py" "$STAGE/$key" "$BUCKET" "$key" \
      "$(python3 "$ROOT/deploy/r2-put.py" --type "$key")"
  done < "$MULTIPART"
fi
echo "sync-blobs: put $PUT_COUNT, multipart $MULTIPART_COUNT, skip $SKIPPED ($BUCKET)"
