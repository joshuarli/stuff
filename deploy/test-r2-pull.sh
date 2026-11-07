#!/bin/sh
# Exercise r2-pull.py against the local R2 simulation (never real R2).
# Checks: only the current generation and the shared stores are pulled,
# bytes match the synced site, the current and
# wheels links are restored, and a non-empty destination is refused.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/r2-pull-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
SITE=$WORK/site
export CF_LOCAL_STATE="$WORK/state"
export STUFF_STATE_HOME="$WORK/home"
mkdir -p "$SITE/generations/g1/simple" "$SITE/generations/g2" "$SITE/wheels" "$SITE/blobs"
echo '{"gen":1}' > "$SITE/generations/g1/catalog.json"
echo '<html>1</html>' > "$SITE/generations/g1/simple/index.html"
echo '{"gen":0}' > "$SITE/generations/g2/catalog.json"
printf 'wheel bytes\n' > "$SITE/wheels/a-1-py3-none-any.whl"
printf 'blob bytes\n' > "$SITE/blobs/0000.crate"
ln -s ../../wheels "$SITE/generations/g1/wheels"
ln -s generations/g1 "$SITE/current"
python3 - "$SITE" "$WORK/manifest.json" <<'PY'
import hashlib, json, pathlib, sys
site = pathlib.Path(sys.argv[1])
keys = ["generations/g1/catalog.json", "generations/g1/simple/index.html", "wheels/a-1-py3-none-any.whl", "blobs/0000.crate"]
objects = []
for key in keys:
    body = (site / key).read_bytes()
    objects.append({"key": key, "size": len(body), "etag": hashlib.md5(body).hexdigest()})
pathlib.Path(sys.argv[2]).write_text(json.dumps({"schemaVersion": 1, "objects": objects}, indent=2))
PY
printf g1 > "$WORK/current"
put() {
  cf r2 objects put "$1" --file "$2" --bucket-name t --local --persist-to "$CF_LOCAL_STATE" >/dev/null
}
for key in generations/g1/catalog.json generations/g1/simple/index.html generations/g2/catalog.json wheels/a-1-py3-none-any.whl blobs/0000.crate; do
  put "$key" "$SITE/$key"
done
put manifests/g1.json "$WORK/manifest.json"
put current "$WORK/current"

check() { [ "$1" = "$2" ] || { echo "FAIL: $3: got '$1', want '$2'"; exit 1; }; }

"$HERE/r2-pull.py" t "$WORK/dest" > "$WORK/pull.log"
check "$(readlink "$WORK/dest/current")" "generations/g1" "current link"
check "$(readlink "$WORK/dest/generations/g1/wheels")" "../../wheels" "wheels link"
check "$(cat "$WORK/dest/generations/g1/catalog.json")" '{"gen":1}' "generation file"
check "$(cat "$WORK/dest/generations/g1/simple/index.html")" '<html>1</html>' "nested generation file"
check "$(cat "$WORK/dest/wheels/a-1-py3-none-any.whl")" "wheel bytes" "wheel store"
check "$(cat "$WORK/dest/blobs/0000.crate")" "blob bytes" "blob store"
check "$(cat "$WORK/dest/manifests/g1.json" | grep -c '"key"')" "4" "manifest lists the generation objects"
[ ! -e "$WORK/dest/generations/g2" ] || { echo "FAIL: pulled a non-current generation"; exit 1; }
[ -z "$(find "$WORK/dest" -name '*.part')" ] || { echo "FAIL: partial files left behind"; exit 1; }
grep -q '^pulled 5 objects' "$WORK/pull.log" || { echo "FAIL: summary: $(cat "$WORK/pull.log")"; exit 1; }
grep -q '^verified 4 manifest objects' "$WORK/pull.log" || { echo "FAIL: manifest verification: $(cat "$WORK/pull.log")"; exit 1; }

if "$HERE/r2-pull.py" t "$WORK/dest" > "$WORK/again.log" 2>&1; then
  echo "FAIL: pulled into a non-empty directory"
  exit 1
fi
echo "ok: r2-pull"
