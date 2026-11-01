#!/bin/sh
# Roll the R2 `current` marker back to an older generation.
# Usage: r2-rollback.sh BUCKET_NAME GENERATION_ID [--promote=cf|s3]
# Env: CF_LOCAL_STATE sets a local simulation dir. Without it, promotion
#      defaults to --promote=s3, a conditional write that needs R2_S3_*
#      (minted from the authenticated `cf` CLI when unset). --promote=cf
#      writes the marker through `cf` with a plain put.
set -eu
BUCKET=$1
GEN_ID=$2
shift 2
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
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
    fi
  fi
  case $PROMOTE in
    cf)
      export R2_PROMOTE_UNCONDITIONAL=1
      echo "note: promoting through cf without a conditional write; a competing publisher would not be detected"
      ;;
    s3)
      unset R2_PROMOTE_UNCONDITIONAL
      . "$SCRIPT_DIR/r2-s3-credentials.sh"
      mint_s3_credentials "$BUCKET" || {
        echo "FAIL: --promote=s3 needs R2_S3_* and minting them failed; mint them with: eval \"\$(python3 $SCRIPT_DIR/r2-temp-credentials.py $BUCKET)\", or pass --promote=cf" >&2
        exit 1
      }
      ;;
  esac
fi
. "$SCRIPT_DIR/publish-lock.sh"
acquire_publish_lock "$(bucket_lock_path "$BUCKET")"
trap 'release_publish_lock' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
python3 "$SCRIPT_DIR/r2-promote.py" "$BUCKET" "$GEN_ID"
record_promoted_generation "$BUCKET" "$GEN_ID"
