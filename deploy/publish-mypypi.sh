#!/bin/sh
# Publish Python packages to our private wheel index and sync the site to R2.
# Called from the source repos' Makefiles, which own the package list. Runs
# the whole flow in one shell under `set -eu` so a failed step aborts before
# any R2 write.
#
# Required env: PACKAGES="name==version ..." (space-separated requirements).
# Optional env (defaults shown):
#   STUFF_DIR=../stuff PUBLIC_URL=https://pypi.17166969.xyz
#   BUCKET=stuff-mypypi
#   STUFF_STATE_HOME=${XDG_STATE_HOME:-$HOME/.local/state}/stuff
#   STATE_DIR=$STUFF_STATE_HOME/mypypi/state SITE_DIR=$STUFF_STATE_HOME/mypypi/site
#   CATALOG_FILE=$STUFF_STATE_HOME/mypypi/packages.toml
#   CONFIG_FILE=$STUFF_STATE_HOME/mypypi/pyproject.toml
#   LOCK_DIR=$STUFF_STATE_HOME/mypypi/publish.lock
#   PLATFORM=<host platform> PYTHON_REQ=">=3.15,<3.16" (exactly one minor;
#   tracks the pinned CPython — mypypi builds one interpreter's wheels)
#   PYTHON=<interpreter> (defaults to `uv python find "$PYTHON_REQ"`)
#
# State lives outside the calling checkout and the whole transaction is
# serialized, so publishing from another source repository extends one index.
set -eu
STUFF_DIR=${STUFF_DIR:-../stuff}
PUBLIC_URL=${PUBLIC_URL:-https://pypi.17166969.xyz}
BUCKET=${BUCKET:-stuff-mypypi}
STUFF_STATE_HOME=${STUFF_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/stuff}
STATE_DIR=${STATE_DIR:-$STUFF_STATE_HOME/mypypi/state}
SITE_DIR=${SITE_DIR:-$STUFF_STATE_HOME/mypypi/site}
CATALOG_FILE=${CATALOG_FILE:-$STUFF_STATE_HOME/mypypi/packages.toml}
CONFIG_FILE=${CONFIG_FILE:-$STUFF_STATE_HOME/mypypi/pyproject.toml}
LOCK_DIR=${LOCK_DIR:-$STUFF_STATE_HOME/mypypi/publish.lock}
PYTHON_REQ=${PYTHON_REQ:-">=3.15,<3.16"}

. "$STUFF_DIR/deploy/publish-lock.sh"
LEGACY_STATE_DIR=${LEGACY_STATE_DIR:-$PWD/.work/registry-pub/state}
if { [ ! -d "$STATE_DIR" ] || [ -z "$(ls -A "$STATE_DIR" 2>/dev/null)" ]; } && [ -d "$LEGACY_STATE_DIR" ] && [ "$STATE_DIR" != "$LEGACY_STATE_DIR" ]; then
  echo "publish-mypypi: legacy per-checkout state exists at $LEGACY_STATE_DIR" >&2
  echo "publish-mypypi: move it to the canonical $STATE_DIR (or set STATE_DIR explicitly) so another repository cannot hide its packages" >&2
  exit 1
fi
mkdir -p "$STUFF_STATE_HOME/mypypi"
acquire_publish_lock "$LOCK_DIR"
trap 'release_publish_lock' EXIT INT TERM HUP

MYPYPI_BIN=$STUFF_DIR/target/debug/mypypi
R2_SYNC=$STUFF_DIR/deploy/r2-sync.sh
PRUNE=$STUFF_DIR/deploy/prune-generations.sh

if [ -n "${REPUBLISH:-}" ]; then
  cargo build -p mypypi --manifest-path "$STUFF_DIR/Cargo.toml"
  "$MYPYPI_BIN" republish --output "$SITE_DIR" --public-url "$PUBLIC_URL"
  "$R2_SYNC" "$SITE_DIR" "$BUCKET"
  "$PRUNE" "$SITE_DIR"
  exit 0
fi

[ -n "${PACKAGES:-}" ] || { echo "publish-mypypi: PACKAGES is empty" >&2; exit 1; }
command -v uv >/dev/null 2>&1 || { echo "publish-mypypi: uv is not installed" >&2; exit 1; }
if [ -z "${PYTHON:-}" ]; then
  PYTHON=$(uv python find "$PYTHON_REQ")
fi
export PYTHON
"$PYTHON" -c 'import sys; print("publish-mypypi: CPython", ".".join(map(str, sys.version_info[:3])))' >&2

if [ -z "${PLATFORM:-}" ]; then
  UNAME_S=$(uname -s)
  UNAME_M=$(uname -m)
  case "$UNAME_S-$UNAME_M" in
    Linux-x86_64) PLATFORM="linux-x86_64" ;;
    Linux-aarch64|Linux-arm64) PLATFORM="linux-aarch64" ;;
    Darwin-x86_64) PLATFORM="macos-x86_64" ;;
    Darwin-arm64|Darwin-arm64e) PLATFORM="macos-arm64" ;;
    *) echo "publish-mypypi: unsupported host $UNAME_S-$UNAME_M" >&2; exit 1 ;;
  esac
fi

cargo build -p mypypi --manifest-path "$STUFF_DIR/Cargo.toml"
mkdir -p "$STATE_DIR" "$SITE_DIR" "$(dirname "$CATALOG_FILE")" "$(dirname "$CONFIG_FILE")"
{
  echo '[[packages]]'
  for p in $PACKAGES; do
    name=${p%%==*}
    version=${p#*==}
    echo "name = \"$name\""
    echo "version = \"$version\""
    echo '[[packages]]'
  done
} | sed '$d' > "$CATALOG_FILE"
cat > "$CONFIG_FILE" <<EOF
[project]
name = "registry-pub"
version = "0.1.0"
requires-python = "$PYTHON_REQ"

[tool.mypypi]
public_base_url = "$PUBLIC_URL"

[tool.mypypi.targets]
platforms = ["linux-x86_64", "linux-aarch64", "macos-x86_64", "macos-arm64"]
linux_repair_policy = "manylinux_2_28"
EOF
M="--catalog $CATALOG_FILE --config $CONFIG_FILE --state-dir $STATE_DIR"
# shellcheck disable=SC2086
"$MYPYPI_BIN" $M add $PACKAGES
"$MYPYPI_BIN" $M --output "$SITE_DIR" --platform "$PLATFORM" build
"$MYPYPI_BIN" $M --output "$SITE_DIR" --platform "$PLATFORM" validate
"$MYPYPI_BIN" $M --output "$SITE_DIR" --platform "$PLATFORM" publish
"$R2_SYNC" "$SITE_DIR" "$BUCKET"
"$PRUNE" "$SITE_DIR"
