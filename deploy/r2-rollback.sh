#!/bin/sh
# Roll the R2 `current` marker back to an older generation.
# Usage: r2-rollback.sh BUCKET_NAME GENERATION_ID
# Env: CF_LOCAL_STATE sets a local simulation dir; otherwise R2_S3_* is
#      required for the conditional promotion.
#
# The target generation must exist remotely with a stuff-manifest.json. Every
# object in that manifest is verified before the marker moves; a generation
# that was never fully uploaded fails without touching `current`. Rolling
# forward again is the same command with the newer generation id.
set -eu
BUCKET=$1
GEN_ID=$2
case $GEN_ID in
  *[!A-Za-z0-9_-]* | "") echo "FAIL: bad generation id: $GEN_ID"; exit 1 ;;
esac
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
python3 "$SCRIPT_DIR/r2-promote.py" "$BUCKET" "$GEN_ID"
