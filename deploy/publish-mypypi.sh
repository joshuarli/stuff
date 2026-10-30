#!/bin/sh
# Publish Python packages to our private wheel index and sync the site to R2.
# Called from the source repos' Makefiles, which own the package list. Runs
# the whole flow in one shell under `set -eu` so a failed step aborts before
# any R2 write.
#
# Required env: PACKAGES="name==version ..." (space-separated requirements).
# Optional env (defaults shown):
#   STUFF_DIR=../stuff
#   BUCKET=stuff-mypypi STATE_DIR=.work/registry-pub/state SITE_DIR=.work/registry-pub/site
#   CATALOG_FILE=.work/registry-pub/packages.toml CONFIG_FILE=.work/registry-pub/pyproject.toml
#   PLATFORM=<host platform> PYTHON_REQ=">=3.15,<3.16" (exactly one minor;
#   tracks the pinned CPython — mypypi builds one interpreter's wheels)
set -eu
STUFF_DIR=${STUFF_DIR:-../stuff}
BUCKET=${BUCKET:-stuff-mypypi}
STATE_DIR=${STATE_DIR:-.work/registry-pub/state}
SITE_DIR=${SITE_DIR:-.work/registry-pub/site}
CATALOG_FILE=${CATALOG_FILE:-.work/registry-pub/packages.toml}
CONFIG_FILE=${CONFIG_FILE:-.work/registry-pub/pyproject.toml}
PYTHON_REQ=${PYTHON_REQ:-">=3.15,<3.16"}

MYPYPI_BIN=$STUFF_DIR/target/debug/mypypi
R2_SYNC=$STUFF_DIR/deploy/r2-sync.sh

[ -n "${PACKAGES:-}" ] || { echo "publish-mypypi: PACKAGES is empty" >&2; exit 1; }
command -v uv >/dev/null 2>&1 || { echo "publish-mypypi: uv is not installed" >&2; exit 1; }

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
