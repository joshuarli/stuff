#!/bin/sh
# Install the pinned native wheel repair tool into the build interpreter.
# usage: provision-mypypi-repair.sh
# Optional env: PYTHON (defaults to python3)
set -eu
ROOT=$(cd "$(dirname "$0")" && pwd)
PINS=$ROOT/mypypi-repair-versions
PYTHON=${PYTHON:-python3}

fail() { echo "FAIL: $1" >&2; exit 1; }

case "$PYTHON" in
  /*) ;;
  *) PYTHON=$(command -v "$PYTHON") || fail "interpreter not found: $PYTHON" ;;
esac

case "$(uname -s)" in
  Linux)
    tool=auditwheel
    module=auditwheel
    command -v patchelf >/dev/null 2>&1 || fail "patchelf is required by auditwheel"
    command -v readelf >/dev/null 2>&1 || fail "readelf is required by auditwheel"
    ;;
  Darwin)
    tool=delocate
    module=delocate.cmd.delocate_wheel
    ;;
  *)
    fail "unsupported repair host $(uname -s)"
    ;;
esac

version=$(sed -n "s/^$tool //p" "$PINS")
[ -n "$version" ] || fail "$PINS does not pin $tool"
uv pip install --break-system-packages --python "$PYTHON" --quiet "$tool==$version" \
  || fail "could not install $tool $version"
"$PYTHON" -c "import $module" || fail "$tool $version is not importable"
echo "OK: $tool $version installed for $("$PYTHON" -c 'import sys; print(sys.executable)')"
