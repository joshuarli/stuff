#!/bin/sh
# Check that pruning keeps only the active generation and its manifest, and
# removes nothing when the site has no current generation.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/prune-generations-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

SITE=$WORK/site
mkdir -p "$SITE/generations/current" "$SITE/generations/old" "$SITE/manifests"
echo current > "$SITE/generations/current/catalog.json"
echo old > "$SITE/generations/old/catalog.json"
echo '{}' > "$SITE/manifests/current.json"
echo '{}' > "$SITE/manifests/old.json"
ln -s generations/current "$SITE/current"

"$HERE/prune-generations.sh" "$SITE"

[ -d "$SITE/generations/current" ] || { echo "FAIL: removed the active generation"; exit 1; }
[ -f "$SITE/manifests/current.json" ] || { echo "FAIL: removed the active manifest"; exit 1; }
[ ! -e "$SITE/generations/old" ] || { echo "FAIL: kept an inactive generation"; exit 1; }
[ ! -e "$SITE/manifests/old.json" ] || { echo "FAIL: kept an inactive manifest"; exit 1; }

SITE=$WORK/unpublished
mkdir -p "$SITE/generations/only" "$SITE/manifests"
echo keep > "$SITE/generations/only/catalog.json"
echo '{}' > "$SITE/manifests/only.json"

"$HERE/prune-generations.sh" "$SITE"

[ -f "$SITE/generations/only/catalog.json" ] || { echo "FAIL: pruned without a current generation"; exit 1; }
[ -f "$SITE/manifests/only.json" ] || { echo "FAIL: pruned a manifest without a current generation"; exit 1; }

mkdir -p "$WORK/empty"
"$HERE/prune-generations.sh" "$WORK/empty"
echo "ok: prune-generations"
