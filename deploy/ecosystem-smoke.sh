#!/bin/sh
# End-to-end ecosystem smoke: drive the Rust registry binaries with real
# package managers (npm for mynpm, uv/CPython for mypypi) and assert that
# installation from the published sites works.
#
# Requirements: node with npm@12.2.1, uv with CPython 3.15, network access.
# Usage: deploy/ecosystem-smoke.sh [WORKDIR]
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK_IN=${1:-"$ROOT/.work/smoke"}
mkdir -p "$WORK_IN"
WORK=$(cd "$WORK_IN" && pwd)
rm -rf "$WORK"
mkdir -p "$WORK"

fail() { echo "FAIL: $1"; exit 1; }

NODE_VERSION=$(node --version 2>/dev/null || echo "none")
NPM_VERSION=$(npm --version 2>/dev/null || echo "none")
echo "node $NODE_VERSION npm $NPM_VERSION uv $(uv --version 2>/dev/null || echo none)"

# --- mynpm: add/fetch/publish/validate with real npm ---
MYNPM_WORK="$WORK/mynpm"
mkdir -p "$MYNPM_WORK/state" "$MYNPM_WORK/site"
UNAME_S=$(uname -s)
UNAME_M=$(uname -m)
NODE_DOTLESS=$(node -e 'console.log(process.version.slice(1))')
sed "s/26\.12\.0/$NODE_DOTLESS/" "$ROOT/tests/fixtures/mynpm/mynpm.json" > "$MYNPM_WORK/mynpm.json"

(cd "$ROOT" && cargo run -q -p mynpm -- --config "$MYNPM_WORK/mynpm.json" --state-dir "$MYNPM_WORK/state" add --manager npm "is-odd@3.0.1") || fail "mynpm add"
(cd "$ROOT" && cargo run -q -p mynpm -- --config "$MYNPM_WORK/mynpm.json" --state-dir "$MYNPM_WORK/state" fetch) || fail "mynpm fetch"
OUT=$(cd "$ROOT" && cargo run -q -p mynpm -- --config "$MYNPM_WORK/mynpm.json" --state-dir "$MYNPM_WORK/state" publish --site-dir "$MYNPM_WORK/site") || fail "mynpm publish"
echo "$OUT" | grep -q "Published generation" || fail "mynpm publish output: $OUT"
GEN_1=$(echo "$OUT" | sed 's/.*generation //;s/ .*//')
OUT2=$(cd "$ROOT" && cargo run -q -p mynpm -- --config "$MYNPM_WORK/mynpm.json" --state-dir "$MYNPM_WORK/state" publish --site-dir "$MYNPM_WORK/site") || fail "mynpm republish"
echo "$OUT2" | grep -q "Already published generation $GEN_1" || fail "mynpm republish not noop: $OUT2"
(cd "$ROOT" && cargo run -q -p mynpm -- --config "$MYNPM_WORK/mynpm.json" --state-dir "$MYNPM_WORK/state" validate --client npm) || fail "mynpm validate npm"
(cd "$ROOT" && cargo run -q -p stuff -- ls --npm-dir "$MYNPM_WORK/site/mirror" | grep -q "is-odd") || fail "published is-odd not visible in stuff ls"
(cd "$ROOT" && cargo run -q -p stuff -- doctor --npm-dir "$MYNPM_WORK/site/mirror") || fail "stuff doctor failed on published npm site"
echo "smoke: mynpm npm flow green (generation $GEN_1)"

if [ "$(pnpm --version 2>/dev/null || echo none)" = "12.15.0" ]; then
  (cd "$ROOT" && cargo run -q -p mynpm -- --config "$MYNPM_WORK/mynpm.json" --state-dir "$MYNPM_WORK/state" add --manager pnpm "is-odd@3.0.1") || fail "mynpm add pnpm"
  (cd "$ROOT" && cargo run -q -p mynpm -- --config "$MYNPM_WORK/mynpm.json" --state-dir "$MYNPM_WORK/state" fetch) || fail "mynpm fetch pnpm"
  (cd "$ROOT" && cargo run -q -p mynpm -- --config "$MYNPM_WORK/mynpm.json" --state-dir "$MYNPM_WORK/state" publish --site-dir "$MYNPM_WORK/site") || fail "mynpm publish pnpm"
  (cd "$ROOT" && cargo run -q -p mynpm -- --config "$MYNPM_WORK/mynpm.json" --state-dir "$MYNPM_WORK/state" validate --client pnpm) || fail "mynpm validate pnpm"
  echo "smoke: mynpm pnpm flow green"
else
  echo "skip: pnpm 12.15.0 not installed ($(pnpm --version 2>/dev/null || echo none)), pnpm flow not exercised"
fi

# --- mypypi: add/build/validate/publish with uv, then install ---
PYPI_WORK="$WORK/mypypi"
mkdir -p "$PYPI_WORK/state" "$PYPI_WORK/out"
case "$UNAME_S-$UNAME_M" in
  Linux-x86_64) HOST_PLATFORM="linux-x86_64" ;;
  Linux-aarch64|Linux-arm64) HOST_PLATFORM="linux-aarch64" ;;
  Darwin-arm64) HOST_PLATFORM="macos-arm64" ;;
  Darwin-x86_64) HOST_PLATFORM="macos-x86_64" ;;
  *) fail "unsupported host $UNAME_S-$UNAME_M" ;;
esac
cat > "$PYPI_WORK/pyproject.toml" <<'EOF'
[project]
name = "smoke"
version = "0.1.0"
requires-python = ">=3.15,<3.16"

[tool.mypypi.targets]
platforms = ["linux-x86_64", "linux-aarch64", "macos-x86_64", "macos-arm64"]
linux_repair_policy = "manylinux_2_28"
EOF
cat > "$PYPI_WORK/packages.toml" <<'EOF'
[[packages]]
name = "six"
version = "1.16.0"
EOF
M="--catalog $PYPI_WORK/packages.toml --config $PYPI_WORK/pyproject.toml --state-dir $PYPI_WORK/state"
(cd "$ROOT" && cargo run -q -p mypypi -- $M add "six==1.16.0") || fail "mypypi add"
(cd "$ROOT" && cargo run -q -p mypypi -- $M --output "$PYPI_WORK/out" build) || fail "mypypi build"
(cd "$ROOT" && cargo run -q -p mypypi -- $M --output "$PYPI_WORK/out" --platform "$HOST_PLATFORM" validate) || fail "mypypi validate"
(cd "$ROOT" && cargo run -q -p mypypi -- --catalog "$PYPI_WORK/packages.toml" --config "$PYPI_WORK/pyproject.toml" --state-dir "$PYPI_WORK/state" --output "$PYPI_WORK/out" --platform "$HOST_PLATFORM" publish) || fail "mypypi publish"
(cd "$ROOT" && cargo run -q -p stuff -- ls --pypi-dir "$PYPI_WORK/out" | grep -q "six") || fail "published six not visible in stuff ls"
(cd "$ROOT" && cargo run -q -p stuff -- doctor --pypi-dir "$PYPI_WORK/out" --sample-artifacts) || fail "stuff doctor failed on published pypi index"
uv venv -q --python 3.15 "$PYPI_WORK/.venv" || fail "uv venv"
VIRTUAL_ENV="$PYPI_WORK/.venv" uv pip install -q --python 3.15 --index-url "file://$PYPI_WORK/out/current/simple/" --only-binary :all: "six==1.16.0" || fail "uv pip install from published index"
VIRTUAL_ENV="$PYPI_WORK/.venv" uv run -q --python 3.15 --no-sync --project "$PYPI_WORK" python -c "import six, sys; assert six.__version__ == '1.16.0', six.__version__; print('six', six.__version__)" || fail "import six"
echo "smoke: mypypi uv flow green"
echo "OK: ecosystem smoke green"
