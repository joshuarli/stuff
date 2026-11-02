#!/bin/sh
# Build a native C and C++ sdist through mypypi's build, repair, validation,
# and publication path, then install and import the published wheel.
#
# usage: test-mypypi-native.sh [WORKDIR]
# Optional env: PYTHON, MYPYPI_EXPECT_MANYLINUX (defaults to 2.28; Linux hosts
# must match it because a newer glibc cannot produce the promised floor).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK_IN=${1:-"$ROOT/.work/native-smoke"}
mkdir -p "$WORK_IN"
WORK=$(cd "$WORK_IN" && pwd)
rm -rf "$WORK"
mkdir -p "$WORK"

fail() { echo "FAIL: $1" >&2; exit 1; }

if [ -z "${PYTHON:-}" ]; then
  PYTHON=$(uv python find 3.15) || fail "CPython 3.15 is not installed"
fi
export PYTHON

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) PLATFORM=linux-x86_64 ;;
  Linux-aarch64|Linux-arm64) PLATFORM=linux-aarch64 ;;
  Darwin-arm64) PLATFORM=macos-arm64 ;;
  Darwin-x86_64) PLATFORM=macos-x86_64 ;;
  *) fail "unsupported host $(uname -s)-$(uname -m)" ;;
esac

case "$PLATFORM" in
  linux-*)
    host_glibc=$(ldd --version | head -1 | awk '{print $NF}')
    expected_glibc=${MYPYPI_EXPECT_MANYLINUX:-2.28}
    [ "$host_glibc" = "$expected_glibc" ] \
      || fail "native Linux wheels must build on glibc $expected_glibc (host glibc $host_glibc); run make test-mypypi-linux-native"
    ;;
esac

STATE="$WORK/state"
OUT="$WORK/out"
mkdir -p "$STATE/wheels"
cat > "$WORK/pyproject.toml" <<EOF
[project]
name = "native-smoke"
version = "0.1.0"
requires-python = ">=3.15,<3.16"

[tool.mypypi.targets]
platforms = ["$PLATFORM"]
linux_repair_policy = "manylinux_2_28"
EOF
cat > "$WORK/packages.toml" <<'EOF'
[[packages]]
name = "native-probe"
version = "0.1.0"
EOF

(cd "$ROOT" && MYPYPI_NATIVE_STATE="$STATE" cargo test -q -p mypypi --lib \
  native_sdist_builds_a_repaired_extension_wheel -- --ignored) \
  || fail "native sdist build"

WHEELS=$(find "$STATE/wheels" -maxdepth 1 -name 'native_probe-*.whl' -type f)
COUNT=$(printf '%s\n' "$WHEELS" | grep -c . || true)
[ "$COUNT" -eq 1 ] || fail "expected one built native wheel, found $COUNT"
WHEEL=$WHEELS

case "$PLATFORM" in
  linux-*)
    case "$(basename "$WHEEL")" in
      *-manylinux_2_28_*) ;;
      *) fail "wheel was not repaired to manylinux_2_28: $WHEEL" ;;
    esac
    mkdir -p "$WORK/unpacked"
    python3 -m zipfile -e "$WHEEL" "$WORK/unpacked"
    : > "$WORK/glibc-floors"
    for module in "$WORK"/unpacked/native_probe*.so; do
      floor=$(readelf --version-info "$module" \
        | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -Vu | tail -1)
      [ -n "$floor" ] || fail "$(basename "$module") has no GLIBC symbol versions"
      printf '%s\n' "$floor" >> "$WORK/glibc-floors"
    done
    highest=$(sort -Vu "$WORK/glibc-floors" | tail -1)
    [ "$highest" = "2.28" ] \
      || fail "highest GLIBC requirement is ${highest:-unknown}, not the 2.28 floor"
    "$PYTHON" -m auditwheel show "$WHEEL" > "$WORK/auditwheel-show.txt" \
      || fail "auditwheel show rejected the repaired wheel"
    grep -q 'manylinux_2_28' "$WORK/auditwheel-show.txt" \
      || fail "auditwheel does not report the wheel as manylinux_2_28"
    ;;
  macos-*)
    case "$(basename "$WHEEL")" in
      *-macosx_15_0_*) ;;
      *) fail "wheel does not carry the macOS 15.0 deployment floor: $WHEEL" ;;
    esac
    mkdir -p "$WORK/unpacked"
    python3 -m zipfile -e "$WHEEL" "$WORK/unpacked"
    for module in "$WORK"/unpacked/native_probe*.so; do
      if otool -L "$module" | grep -Eq '/opt/homebrew|/usr/local'; then
        fail "$(basename "$module") still links a host library: $(otool -L "$module")"
      fi
      minos=$(otool -l "$module" \
        | awk '/LC_BUILD_VERSION/{found=1; next} found && $1=="minos"{print $2; exit}')
      if [ -z "$minos" ]; then
        minos=$(otool -l "$module" \
          | awk '/LC_VERSION_MIN_MACOSX/{found=1; next} found && $1=="version"{print $2; exit}')
      fi
      [ "$minos" = "15.0" ] \
        || fail "$(basename "$module") has minos ${minos:-unknown}, not 15.0"
    done
    ;;
esac

M="--catalog $WORK/packages.toml --config $WORK/pyproject.toml --state-dir $STATE"
(cd "$ROOT" && cargo run -q -p mypypi -- $M --output "$OUT" --platform "$PLATFORM" validate) \
  || fail "mypypi validate"
(cd "$ROOT" && cargo run -q -p mypypi -- $M --output "$OUT" --platform "$PLATFORM" publish) \
  || fail "mypypi publish"

uv venv -q --python "$PYTHON" "$WORK/.venv" || fail "uv venv"
VIRTUAL_ENV="$WORK/.venv" uv pip install -q --python "$WORK/.venv/bin/python" \
  --index-url "file://$OUT/current/simple/" --only-binary :all: "native-probe==0.1.0" \
  || fail "install from published index"
"$WORK/.venv/bin/python" -I -c \
  'import native_probe, native_probe_cpp; assert native_probe.answer() == 42; assert native_probe_cpp.answer() == 42' \
  || fail "import native extensions"
echo "OK: native wheel build, repair, validation, publication, and install green ($(basename "$WHEEL"))"
