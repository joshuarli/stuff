#!/bin/sh
# Upload one site file as one R2 object. Called by r2-sync.sh (xargs -P).
# Usage: r2-put.sh KEY | r2-put.sh --type KEY (print the content type only)
# Env: SITE_DIR, BUCKET, CF_LOCAL_STATE (optional local simulation dir).
set -eu
if [ "$1" = "--type" ]; then
  KEY=$2
  SHOW_TYPE=1
else
  KEY=$1
  SHOW_TYPE=""
fi
case $KEY in
  *.json) TYPE='application/json' ;;
  *.html) TYPE='text/html; charset=utf-8' ;;
  *.txt | *.md) TYPE='text/markdown; charset=utf-8' ;;
  *.toml) TYPE='text/plain; charset=utf-8' ;;
  *.crate) TYPE='application/x-tar' ;;
  *.tgz | *.whl | *.gz) TYPE='application/octet-stream' ;;
  *.metadata) TYPE='text/plain; charset=utf-8' ;;
  *) TYPE='application/octet-stream' ;;
esac
if [ -n "$SHOW_TYPE" ]; then
  printf '%s' "$TYPE"
  exit 0
fi
if [ -n "${CF_LOCAL_STATE:-}" ]; then
  cf r2 objects put "$KEY" --bucket-name "$BUCKET" --file "$SITE_DIR/$KEY" \
    --content-type "$TYPE" --local --persist-to "$CF_LOCAL_STATE" >/dev/null
else
  cf r2 objects put "$KEY" --bucket-name "$BUCKET" --file "$SITE_DIR/$KEY" \
    --content-type "$TYPE" >/dev/null
fi
echo "put $KEY"
