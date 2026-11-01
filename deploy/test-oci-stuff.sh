#!/bin/sh
# Build a fixture OCI site, then exercise stuff's OCI listing, search,
# rendering, and doctor checks against it.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-"$ROOT/.work/oci-stuff"}
TMPDIR=${TMPDIR:-"$ROOT/.work/tmp"}
export TMPDIR
mkdir -p "$TMPDIR"
rm -rf "$WORK"
mkdir -p "$WORK"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}
command -v jaq >/dev/null || fail "jaq is required"

(cd "$ROOT" && cargo build -q -p myoci -p stuff)
TARGET=${CARGO_TARGET_DIR:-"$ROOT/target"}
MYOCI=$TARGET/debug/myoci
STUFF=$TARGET/debug/stuff
STATE=$WORK/state
SITE=$WORK/site

"$MYOCI" init --state-dir "$STATE" >/dev/null
"$MYOCI" import --state-dir "$STATE" --layout "$ROOT/tests/fixtures/oci/layout" --repository example/app >/dev/null
"$MYOCI" publish --state-dir "$STATE" --site-dir "$SITE" >/dev/null

"$STUFF" ls --registry oci --oci-dir "$SITE" > "$WORK/ls.txt"
grep -q "example/app" "$WORK/ls.txt" || fail "ls did not list example/app"

"$STUFF" search example --registry oci --oci-dir "$SITE" --json > "$WORK/search.json"
jaq -e '.catalogs[0].registry == "oci" and (.catalogs[0].packages | length) == 1' "$WORK/search.json" >/dev/null \
  || fail "search did not return the OCI package"

"$STUFF" render --registry oci --oci-dir "$SITE" --output "$WORK/oci.html" >/dev/null
grep -q "example/app" "$WORK/oci.html" || fail "render omitted example/app"

"$STUFF" render --registry oci --oci-dir "$SITE" --output "$WORK/oci.txt" >/dev/null
grep -q "OCI registry" "$WORK/oci.txt" || fail "llms render omitted the OCI registry"
grep -q "OCI: " "$WORK/oci.txt" || fail "llms render omitted the OCI origin"

"$STUFF" doctor --oci-dir "$SITE" > "$WORK/doctor.txt" || {
  cat "$WORK/doctor.txt" >&2
  fail "doctor failed"
}
grep -q "ok: oci manifest example/app:stable" "$WORK/doctor.txt" || fail "doctor did not verify the manifest"
grep -q "ok: oci config example/app:stable" "$WORK/doctor.txt" || fail "doctor did not verify the config blob"
grep -q "FAIL" "$WORK/doctor.txt" && fail "doctor reported a failure"
echo "OK: stuff lists, searches, renders, and verifies the OCI site"
