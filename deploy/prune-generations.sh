#!/bin/sh
# Remove local generations and manifests other than the active generation.
# Remote manifests stay in the bucket, so rollback does not need the local copy.
# Usage: prune-generations.sh SITE_DIR
set -eu
SITE_DIR=$1
current=$(readlink "$SITE_DIR/current" 2>/dev/null || true)
current=${current##*/}
[ -n "$current" ] || exit 0
for gen in "$SITE_DIR"/generations/*; do
  [ -d "$gen" ] || continue
  [ "${gen##*/}" = "$current" ] && continue
  rm -rf "$gen"
done
for manifest in "$SITE_DIR"/manifests/*.json; do
  [ -e "$manifest" ] || continue
  [ "${manifest##*/}" = "$current.json" ] && continue
  rm -f "$manifest"
done
