#!/bin/sh
# Roll the R2 `current` marker back to an older generation.
# Usage: r2-rollback.sh BUCKET_NAME GENERATION_ID [--promote=cf|s3]
# Env: CF_LOCAL_STATE sets a local simulation dir. Without it, promotion
#      defaults to the authenticated `cf` CLI unless all R2_S3_* variables are
#      set; --promote=s3 requires them and uses a conditional write.
#
# The target generation must exist remotely with a stuff-manifest.json. Every
# object in that manifest is verified before the marker moves; a generation
# that was never fully uploaded fails without touching `current`. Rolling
# forward again is the same command with the newer generation id.
set -eu
BUCKET=$1
GEN_ID=$2
shift 2
PROMOTE=""
while [ $# -gt 0 ]; do
  case $1 in
    --promote=cf|--promote=s3)
      PROMOTE=${1#--promote=}
      shift
      ;;
    *)
      echo "FAIL: usage: r2-rollback.sh BUCKET_NAME GENERATION_ID [--promote=cf|s3]"
      exit 1
      ;;
  esac
done
case $GEN_ID in
  *[!A-Za-z0-9_-]* | "") echo "FAIL: bad generation id: $GEN_ID"; exit 1 ;;
esac
if [ -z "${CF_LOCAL_STATE:-}" ]; then
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
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
python3 "$SCRIPT_DIR/r2-promote.py" "$BUCKET" "$GEN_ID"
