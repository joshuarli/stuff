#!/bin/sh
# Prove deploy/uv-version is the only uv version the build, image, docs, and
# CI agree on. Fails when a file names a different version or stops reading
# the pin.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PIN=$(tr -d '[:space:]' < "$ROOT/deploy/uv-version")
printf '%s\n' "$PIN" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || {
  echo "FAIL: deploy/uv-version must hold one major.minor.patch version, found '$PIN'" >&2
  exit 1
}

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

mismatch() {
  fail "$1 names uv $2, but deploy/uv-version pins $PIN"
}

BUILD=$ROOT/crates/mypypi/src/build.rs
README=$ROOT/crates/mypypi/README.md
DOCKERFILE=$ROOT/deploy/mypypi-linux.Dockerfile
CI=$ROOT/.github/workflows/ci.yml

grep -Fq 'include_str!("../../../deploy/uv-version")' "$BUILD" \
  || fail "crates/mypypi/src/build.rs does not include deploy/uv-version"
for version in $(grep -Eo 'uv[^0-9]{0,8}[0-9]+\.[0-9]+\.[0-9]+' "$BUILD" \
  | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | sort -u); do
  [ "$version" = "$PIN" ] || mismatch "crates/mypypi/src/build.rs" "$version"
done

readme_versions=$(grep -Eo 'uv`[[:space:]]+[0-9]+\.[0-9]+\.[0-9]+' "$README" \
  | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | sort -u)
[ -n "$readme_versions" ] || fail "crates/mypypi/README.md does not name the uv pin"
for version in $readme_versions; do
  [ "$version" = "$PIN" ] || mismatch "crates/mypypi/README.md" "$version"
done

for version in $(grep -Eo 'astral\.sh/uv/[0-9]+\.[0-9]+\.[0-9]+' "$DOCKERFILE" | sed 's#.*/##' | sort -u); do
  [ "$version" = "$PIN" ] || mismatch "deploy/mypypi-linux.Dockerfile" "$version"
done
grep -Fq 'COPY uv-version' "$DOCKERFILE" \
  || fail "deploy/mypypi-linux.Dockerfile does not copy deploy/uv-version into the build"
grep -Fq 'https://astral.sh/uv/$UV_VERSION/install.sh' "$DOCKERFILE" \
  || fail "deploy/mypypi-linux.Dockerfile does not install the uv pin"

awk '
  /^  [a-z0-9_-]+:/ { job=$1; sub(/:$/, "", job) }
  /astral-sh\/setup-uv@/ { uv[job]=1; uses++ }
  /UV_VERSION=\$\(cat deploy\/uv-version\)/ { reads[job]=1 }
  END {
    for (job in uv) {
      if (!reads[job]) {
        printf "FAIL: CI job %s installs uv without reading deploy/uv-version\n", job > "/dev/stderr"
        bad=1
      }
    }
    if (uses == 0) {
      print "FAIL: .github/workflows/ci.yml has no astral-sh/setup-uv step" > "/dev/stderr"
      bad=1
    }
    exit bad
  }
' "$CI" || exit 1

ci_versions=$(awk '
  /astral-sh\/setup-uv@/ { uv=1; next }
  uv && /^[[:space:]]*version:/ {
    value=$0
    sub(/^[[:space:]]*version:[[:space:]]*/, "", value)
    sub(/[[:space:]]+$/, "", value)
    print value
    uv=0
  }
  uv && /^[[:space:]]*-[[:space:]]/ { uv=0 }
' "$CI")
uv_steps=$(grep -c 'astral-sh/setup-uv@' "$CI" || true)
uv_versions=$(printf '%s\n' "$ci_versions" | grep -c . || true)
[ "$uv_steps" -eq "$uv_versions" ] \
  || fail ".github/workflows/ci.yml has $uv_steps astral-sh/setup-uv steps but $uv_versions version inputs"
if printf '%s\n' "$ci_versions" | grep -vqxF '${{ env.UV_VERSION }}'; then
  for version in $(printf '%s\n' "$ci_versions" | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' || true); do
    [ "$version" = "$PIN" ] || mismatch ".github/workflows/ci.yml" "$version"
  done
  fail ".github/workflows/ci.yml must pass version: \${{ env.UV_VERSION }} to astral-sh/setup-uv"
fi

echo "OK: uv $PIN matches deploy/uv-version in the build, image, docs, and CI"
