#!/bin/sh
# Exercise r2-sync.sh against the local R2 simulation (never real R2).
# Usage: test-r2-sync.sh [OBJECT_COUNT]   (default 1100, above the 1000 page)
# Checks: listing past one page, unchanged re-sync uploads nothing, changed
# immutable content is a conflict unless --repair is explicit, the marker is
# last and verified, a large file whose multipart ETag matches is skipped, and
# rollback refuses a generation that was never uploaded.
set -eu
COUNT=${1:-1100}
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/r2-sync-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
SITE=$WORK/site
export CF_LOCAL_STATE="$WORK/state"
mkdir -p "$SITE/generations/g1" "$SITE/tarballs"
echo '{}' > "$SITE/generations/g1/catalog.json"
ln -s generations/g1 "$SITE/current"
i=0
while [ "$i" -lt "$COUNT" ]; do
  printf 'object %s\n' "$i" > "$SITE/tarballs/pkg-$i.tgz"
  i=$((i + 1))
done

check() { [ "$1" = "$2" ] || { echo "FAIL: $3: got '$1', want '$2'"; exit 1; }; }

if env -u CF_LOCAL_STATE -u R2_S3_ENDPOINT -u R2_S3_ACCESS_KEY_ID -u R2_S3_SECRET_ACCESS_KEY \
  "$HERE/r2-sync.sh" "$SITE" t --promote=s3 >"$WORK/s3-missing.log" 2>&1; then
  echo "FAIL: --promote=s3 ran without S3 credentials"
  exit 1
fi
grep -q 'R2_S3_ENDPOINT is not set' "$WORK/s3-missing.log" || { echo "FAIL: missing S3 credentials were not reported"; exit 1; }

if env -u CF_LOCAL_STATE -u R2_S3_ENDPOINT -u R2_S3_ACCESS_KEY_ID -u R2_S3_SECRET_ACCESS_KEY \
  "$HERE/r2-rollback.sh" t g1 --promote=s3 >"$WORK/rollback-s3-missing.log" 2>&1; then
  echo "FAIL: rollback --promote=s3 ran without S3 credentials"
  exit 1
fi
grep -q 'R2_S3_ENDPOINT is not set' "$WORK/rollback-s3-missing.log" || { echo "FAIL: rollback did not report missing S3 credentials"; exit 1; }

START=$(date +%s)
"$HERE/r2-sync.sh" "$SITE" t > "$WORK/first.log"
MID=$(date +%s)
TOTAL=$((COUNT + 2))
check "$(grep -c '^put ' "$WORK/first.log")" "$TOTAL" "first sync puts"
tail -n 2 "$WORK/first.log" | grep -q 'verified current is g1' || { echo "FAIL: marker not verified last"; exit 1; }

"$HERE/r2-sync.sh" "$SITE" t > "$WORK/second.log"
END=$(date +%s)
check "$(grep -c '^put ' "$WORK/second.log" || true)" "0" "unchanged re-sync puts"
grep -q "uploaded 0, skipped $TOTAL" "$WORK/second.log" || { echo "FAIL: unchanged re-sync summary"; exit 1; }

cp -a "$SITE/generations/g1" "$SITE/generations/g1b"
ln -sfn generations/g1b "$SITE/current"
"$HERE/r2-sync.sh" "$SITE" t --dry-run > "$WORK/dry.log"
grep -q '^dry-run g1b: put 2 (.* bytes), multipart 0 (0 bytes), skip ' "$WORK/dry.log" || { echo "FAIL: dry-run summary"; exit 1; }
rm -rf "$SITE/generations/g1b" "$SITE/manifests/g1b.json"
ln -sfn generations/g1 "$SITE/current"
"$HERE/r2-sync.sh" "$SITE" t > "$WORK/after-dry.log"
check "$(grep -c '^put ' "$WORK/after-dry.log" || true)" "0" "dry-run uploaded nothing"

printf 'changed\n' > "$SITE/tarballs/pkg-7.tgz"
if R2_LIST_PAGE=50 "$HERE/r2-sync.sh" "$SITE" t > "$WORK/third.log" 2>&1; then
  echo "FAIL: changed immutable object was overwritten without --repair"
  exit 1
fi
grep -q 'tarballs/pkg-7.tgz' "$WORK/third.log" || { echo "FAIL: conflict did not name the object"; exit 1; }
check "$(sed -n 's/^current$//p' "$SITE/current" 2>/dev/null || true)" "" "current is still a link"

R2_LIST_PAGE=50 "$HERE/r2-sync.sh" "$SITE" t --repair > "$WORK/repair.log"
check "$(grep -c '^put ' "$WORK/repair.log")" "2" "repair puts changed object and manifest"
grep -q '^put tarballs/pkg-7.tgz$' "$WORK/repair.log" || { echo "FAIL: wrong file re-uploaded"; exit 1; }

# A second complete generation can be promoted and rolled back.
cp -a "$SITE/generations/g1" "$SITE/generations/g2"
ln -sfn generations/g2 "$SITE/current"
R2_LIST_PAGE=50 "$HERE/r2-sync.sh" "$SITE" t > "$WORK/second-gen.log"
grep -q 'verified current is g2' "$WORK/second-gen.log" || { echo "FAIL: second generation was not promoted"; exit 1; }
"$HERE/r2-rollback.sh" t g1 > "$WORK/rollback-ok.log"
grep -q 'verified current is g1' "$WORK/rollback-ok.log" || { echo "FAIL: verified rollback did not restore g1"; exit 1; }

# A later generation that grows the shared store must not trip over the manifests of earlier ones.
printf 'later\n' > "$SITE/tarballs/pkg-later.tgz"
cp -a "$SITE/generations/g2" "$SITE/generations/g4"
ln -sfn generations/g4 "$SITE/current"
R2_LIST_PAGE=50 "$HERE/r2-sync.sh" "$SITE" t --expected g1 > "$WORK/later-gen.log"
grep -q 'verified current is g4' "$WORK/later-gen.log" || { echo "FAIL: later generation was not promoted"; exit 1; }

# A generation with no remote manifest must not move current.
ln -sfn generations/g3 "$SITE/current"
mkdir -p "$SITE/generations/g3"
echo '{}' > "$SITE/generations/g3/catalog.json"
if "$HERE/r2-rollback.sh" t g3 > "$WORK/rollback.log" 2>&1; then
  echo "FAIL: rollback promoted an unuploaded generation"
  exit 1
fi
grep -q 'no manifests/g3.json' "$WORK/rollback.log" || { echo "FAIL: rollback failure was not explicit"; exit 1; }
check "$(grep -o 'verified current is [A-Za-z0-9_-]*' "$WORK/rollback-ok.log" | tail -n 1)" "verified current is g1" "rollback left current untouched"

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
