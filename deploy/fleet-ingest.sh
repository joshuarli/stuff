#!/bin/sh
# Ingest a fleet's dependencies into local site directories, one per registry.
# Usage: fleet-ingest.sh OUT_DIR [--cargo LIST] [--npm LIST] [--pypi LIST]
#
# Each LIST is a text file, one entry per line (blank lines and # comments
# ignored):
#   --cargo  path to a Cargo.lock
#   --npm    path to a pnpm-lock.yaml
#   --pypi   a requirement "name==version"
# The project name for a lockfile is the name of its directory.
#
# Output (no R2 writes, no network publication):
#   OUT_DIR/mycrates/{state,site}   Cargo sparse registry
#   OUT_DIR/mynpm/{state,site}      npm archive; serve site/mirror
#   OUT_DIR/mypypi/{state,site}     wheel index
# Sync a finished site with r2-sync.sh (LOCAL=1 / CF_LOCAL_STATE to simulate).
#
# Registries are independent: a failure in one is reported at the end and the
# others still run. Exit status is non-zero when any registry failed.
#
# Optional env (defaults shown):
#   STUFF_DIR=<repo root>  BIN_DIR=$STUFF_DIR/target/debug
#   CRATES_URL=https://crates.17166969.xyz NPM_URL=https://npm.17166969.xyz
#   NPM_UPSTREAM=https://registry.npmjs.org/ NPM_TARGETS="" (extra target ids)
#   NPM_CLIENT="" (npm|pnpm: also run client validation, installs packages)
#   PYPI_PLATFORM=<host platform> PYTHON_REQ=">=3.15,<3.16"
set -u
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
STUFF_DIR=${STUFF_DIR:-$(cd "$SCRIPT_DIR/.." && pwd)}
BIN_DIR=${BIN_DIR:-$STUFF_DIR/target/debug}
CRATES_URL=${CRATES_URL:-https://crates.17166969.xyz}
NPM_URL=${NPM_URL:-https://npm.17166969.xyz}
NPM_UPSTREAM=${NPM_UPSTREAM:-https://registry.npmjs.org/}
NPM_TARGETS=${NPM_TARGETS:-}
NPM_CLIENT=${NPM_CLIENT:-}
PYTHON_REQ=${PYTHON_REQ:->=3.15,<3.16}

usage() {
  echo "usage: fleet-ingest.sh OUT_DIR [--cargo LIST] [--npm LIST] [--pypi LIST]" >&2
  exit 2
}

[ $# -ge 1 ] || usage
OUT_DIR=$1
shift
CARGO_LIST=""
NPM_LIST=""
PYPI_LIST=""
while [ $# -gt 0 ]; do
  [ $# -ge 2 ] || usage
  [ -f "$2" ] || { echo "fleet-ingest: no such list: $2" >&2; exit 2; }
  case $1 in
    --cargo) CARGO_LIST=$2 ;;
    --npm) NPM_LIST=$2 ;;
    --pypi) PYPI_LIST=$2 ;;
    *) usage ;;
  esac
  shift 2
done
[ -n "$CARGO_LIST$NPM_LIST$PYPI_LIST" ] || usage

entries() {
  grep -v -e '^[[:space:]]*$' -e '^[[:space:]]*#' "$1"
}

project_of() {
  basename "$(cd "$(dirname "$1")" && pwd)"
}

ingest_cargo() {
  bin=$STUFF_DIR/crates/mycrates/bin/mycrates
  state=$OUT_DIR/mycrates/state
  site=$OUT_DIR/mycrates/site
  mkdir -p "$state" "$site" || return 1
  "$bin" init --state-dir "$state" --site-dir "$site" >/dev/null || return 1
  "$bin" config set-public-base-url "$CRATES_URL" --state-dir "$state" || return 1
  entries "$CARGO_LIST" | while IFS= read -r lock; do
    echo "mycrates import-lock $lock"
    "$bin" import-lock "$lock" --project "$(project_of "$lock")" --state-dir "$state" || exit 1
  done || return 1
  "$bin" fetch --state-dir "$state" || return 1
  "$bin" verify --state-dir "$state" || return 1
  "$bin" publish --state-dir "$state" --site-dir "$site" || return 1
}

ingest_npm() {
  bin=$BIN_DIR/mynpm
  state=$OUT_DIR/mynpm/state
  site=$OUT_DIR/mynpm/site
  config=$OUT_DIR/mynpm/mynpm.json
  mkdir -p "$state" "$site" || return 1
  node_version=$(node -e 'console.log(process.version.slice(1))') || return 1
  "$bin" init-config --node-version "$node_version" --public-url "$NPM_URL" \
    --upstream-registry "$NPM_UPSTREAM" --output "$config" || return 1
  entries "$NPM_LIST" | while IFS= read -r lock; do
    project=$(project_of "$lock")
    echo "mynpm import-lock $lock"
    "$bin" --config "$config" --state-dir "$state" import-lock "$lock" --project "$project" || exit 1
    for t in $NPM_TARGETS; do
      "$bin" --config "$config" --state-dir "$state" import-lock "$lock" --project "$project" --target "$t" || exit 1
    done
  done || return 1
  "$bin" --config "$config" --state-dir "$state" fetch || return 1
  "$bin" --config "$config" --state-dir "$state" verify || return 1
  if [ -n "$NPM_CLIENT" ]; then
    "$bin" --config "$config" --state-dir "$state" validate --client "$NPM_CLIENT" || return 1
  fi
  "$bin" --config "$config" --state-dir "$state" publish --site-dir "$site" || return 1
}

ingest_pypi() {
  bin=$BIN_DIR/mypypi
  state=$OUT_DIR/mypypi/state
  site=$OUT_DIR/mypypi/site
  catalog=$OUT_DIR/mypypi/packages.toml
  config=$OUT_DIR/mypypi/pyproject.toml
  if [ -z "${PYTHON:-}" ]; then
    command -v uv >/dev/null 2>&1 || { echo "fleet-ingest: uv is not installed" >&2; return 1; }
    PYTHON=$(uv python find "$PYTHON_REQ") || return 1
    export PYTHON
  fi
  platform=${PYPI_PLATFORM:-}
  if [ -z "$platform" ]; then
    case "$(uname -s)-$(uname -m)" in
      Linux-x86_64) platform=linux-x86_64 ;;
      Linux-aarch64 | Linux-arm64) platform=linux-aarch64 ;;
      Darwin-x86_64) platform=macos-x86_64 ;;
      Darwin-arm64 | Darwin-arm64e) platform=macos-arm64 ;;
      *) echo "fleet-ingest: unsupported host for mypypi" >&2; return 1 ;;
    esac
  fi
  mkdir -p "$state" "$site" || return 1
  : > "$catalog"
  cat > "$config" <<CONFIG || return 1
[project]
name = "fleet-ingest"
version = "0.1.0"
requires-python = "$PYTHON_REQ"

[tool.mypypi.targets]
platforms = ["linux-x86_64", "linux-aarch64", "macos-x86_64", "macos-arm64"]
linux_repair_policy = "manylinux_2_28"
CONFIG
  reqs=$(entries "$PYPI_LIST" | tr '\n' ' ')
  common="--catalog $catalog --config $config --state-dir $state"
  # shellcheck disable=SC2086
  "$bin" $common add $reqs || return 1
  # shellcheck disable=SC2086
  "$bin" $common --output "$site" --platform "$platform" build || return 1
  # shellcheck disable=SC2086
  "$bin" $common --output "$site" --platform "$platform" validate || return 1
  # shellcheck disable=SC2086
  "$bin" $common --output "$site" --platform "$platform" publish || return 1
}

mkdir -p "$OUT_DIR" || exit 1
cargo build -p mynpm -p mypypi --manifest-path "$STUFF_DIR/Cargo.toml" || exit 1
FAILED=""
run() {
  name=$1
  fn=$2
  start=$(date +%s)
  if "$fn"; then
    echo "fleet-ingest: $name ok in $(($(date +%s) - start))s"
  else
    echo "fleet-ingest: $name FAILED after $(($(date +%s) - start))s" >&2
    FAILED="$FAILED $name"
  fi
}
[ -z "$CARGO_LIST" ] || run mycrates ingest_cargo
[ -z "$NPM_LIST" ] || run mynpm ingest_npm
[ -z "$PYPI_LIST" ] || run mypypi ingest_pypi
[ -z "$FAILED" ] || { echo "fleet-ingest: failed:$FAILED" >&2; exit 1; }
