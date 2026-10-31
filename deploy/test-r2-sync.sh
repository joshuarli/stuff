#!/bin/sh
# Exercise r2-sync.sh against the local R2 simulation (never real R2).
# Usage: test-r2-sync.sh [OBJECT_COUNT]   (default 1100, above the 1000 page)
# Checks: listing past one page, unchanged re-sync uploads nothing, one
# changed file uploads exactly one, the marker is last and verified, and a
# large file whose multipart ETag matches is skipped.
set -eu
COUNT=${1:-1100}
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/r2-sync-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
SITE=$WORK/site
export CF_LOCAL_STATE=$WORK/state
mkdir -p "$SITE/generations/g1" "$SITE/tarballs"
echo '{}' > "$SITE/generations/g1/catalog.json"
ln -s generations/g1 "$SITE/current"
i=0
while [ "$i" -lt "$COUNT" ]; do
  printf 'object %s\n' "$i" > "$SITE/tarballs/pkg-$i.tgz"
  i=$((i + 1))
done

check() { [ "$1" = "$2" ] || { echo "FAIL: $3: got '$1', want '$2'"; exit 1; }; }

START=$(date +%s)
"$HERE/r2-sync.sh" "$SITE" t > "$WORK/first.log"
MID=$(date +%s)
TOTAL=$((COUNT + 1))
check "$(grep -c '^put ' "$WORK/first.log")" "$TOTAL" "first sync puts"
tail -n 2 "$WORK/first.log" | grep -q 'verified current is g1' || { echo "FAIL: marker not verified last"; exit 1; }

"$HERE/r2-sync.sh" "$SITE" t > "$WORK/second.log"
END=$(date +%s)
check "$(grep -c '^put ' "$WORK/second.log" || true)" "0" "unchanged re-sync puts"
grep -q "uploaded 0, skipped $TOTAL" "$WORK/second.log" || { echo "FAIL: unchanged re-sync summary"; exit 1; }

printf 'changed\n' > "$SITE/tarballs/pkg-7.tgz"
R2_LIST_PAGE=50 "$HERE/r2-sync.sh" "$SITE" t > "$WORK/third.log"
check "$(grep -c '^put ' "$WORK/third.log")" "1" "changed file puts"
grep -q '^put tarballs/pkg-7.tgz$' "$WORK/third.log" || { echo "FAIL: wrong file re-uploaded"; exit 1; }

BIG=$WORK/big
mkdir -p "$BIG"
head -c 300 /dev/zero > "$BIG/blob"
printf 'blob\n' > "$BIG/keys"
mkdir -p "$WORK/bin"
ETAG=$(python3 -c '
import hashlib
d = open("'"$BIG"'/blob", "rb").read()
parts = [d[i:i + 128] for i in range(0, len(d), 128)]
print(hashlib.md5(b"".join(hashlib.md5(p).digest() for p in parts)).hexdigest() + "-%d" % len(parts))')
cat > "$WORK/bin/cf" <<FAKE
#!/bin/sh
printf '[{"key":"blob","size":300,"etag":"%s"}]' "$ETAG"
FAKE
chmod +x "$WORK/bin/cf"
PLAN=$(PATH="$WORK/bin:$PATH" R2_SINGLE_PUT_MAX=200 python3 -I -c '
import importlib.util, sys
spec = importlib.util.spec_from_file_location("r2_plan", "'"$HERE"'/r2-plan.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
m.part_size = lambda: 128
sys.argv = ["r2-plan", "'"$BIG"'", "t", "'"$BIG"'/keys"]
m.main()')
check "$PLAN" "skip blob" "unchanged multipart file"
echo "ok: $COUNT objects, first sync $((MID - START))s, unchanged re-sync $((END - MID))s"
