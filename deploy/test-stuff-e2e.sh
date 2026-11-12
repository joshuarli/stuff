#!/bin/sh
# End-to-end clean-machine acceptance test: executes the built `stuff` binary
# through the entire publish path using fake S3 as the R2 backend, pulls the
# live publications with r2-pull.py, serves them through stuff-server, and verifies
# installation with real package managers (npm and uv).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK_IN=${1:-"$ROOT/.work/stuff-e2e"}
mkdir -p "$WORK_IN"
WORK=$(cd "$WORK_IN" && pwd)
rm -rf "$WORK"
mkdir -p "$WORK"

TMPDIR=${TMPDIR:-"$ROOT/.work/tmp"}
export TMPDIR
mkdir -p "$TMPDIR"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

(cd "$ROOT" && cargo build -q -p stuff -p stuff-server)
BIN_DIR=${CARGO_TARGET_DIR:-"$ROOT/target"}/debug
STUFF=$BIN_DIR/stuff
STUFF_SERVER=$BIN_DIR/stuff-server

[ -x "$STUFF" ] || fail "stuff binary not found at $STUFF"
[ -x "$STUFF_SERVER" ] || fail "stuff-server binary not found at $STUFF_SERVER"

# 1. Start fake S3
S3_FIFO=$WORK/fake-s3.stdin
mkfifo "$S3_FIFO"
python3 "$ROOT/deploy/fake_s3.py" <"$S3_FIFO" >"$WORK/fake-s3.out" &
S3_PID=$!
exec 3>"$S3_FIFO"

cleanup() {
  exec 3>&- 2>/dev/null || true
  if [ -n "${PYPI_SERVER_PID:-}" ]; then kill "$PYPI_SERVER_PID" 2>/dev/null || true; fi
  if [ -n "${NPM_SERVER_PID:-}" ]; then kill "$NPM_SERVER_PID" 2>/dev/null || true; fi
  if [ -n "${S3_PID:-}" ]; then wait "$S3_PID" 2>/dev/null || true; fi
}
trap cleanup EXIT

for _ in $(seq 50); do
  grep -q '^PORT ' "$WORK/fake-s3.out" 2>/dev/null && break
  sleep 0.1
done
S3_PORT=$(sed -n 's/^PORT //p' "$WORK/fake-s3.out")
[ -n "$S3_PORT" ] || fail "fake S3 did not start"
ENDPOINT=http://127.0.0.1:$S3_PORT

PYPI_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
NPM_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')

export R2_S3_ENDPOINT="$ENDPOINT" R2_S3_ACCESS_KEY_ID=test R2_S3_SECRET_ACCESS_KEY=secret
export STUFF_CONFIG_HOME="$WORK/config" STUFF_STATE_HOME="$WORK/state" STUFF_CACHE_HOME="$WORK/cache"
mkdir -p "$STUFF_CONFIG_HOME" "$STUFF_STATE_HOME" "$STUFF_CACHE_HOME"

cat > "$STUFF_CONFIG_HOME/config.json" <<EOF
{
  "endpoint_url": "$ENDPOINT",
  "npm": {
    "bucket": "test-npm",
    "public_base_url": "http://127.0.0.1:$NPM_PORT"
  },
  "pypi": {
    "bucket": "test-pypi",
    "public_base_url": "http://127.0.0.1:$PYPI_PORT"
  },
  "oci": {
    "bucket": "test-oci",
    "blobs_bucket": "test-oci-blobs",
    "public_base_url": "http://127.0.0.1:9999"
  }
}
EOF

# 2. PyPI: publish with stuff, pull with r2-pull.py, serve, install with uv
echo "==> Testing PyPI publish and uv installation"
PYPI_OUT=$("$STUFF" publish pypi "six==1.16.0") || fail "stuff publish pypi failed"
echo "$PYPI_OUT" | grep -q "pypi published generation" || fail "unexpected output: $PYPI_OUT"

PYPI_SITE="$WORK/pypi-site"
python3 "$ROOT/deploy/r2-pull.py" test-pypi "$PYPI_SITE" || fail "r2-pull.py failed for pypi"
[ -d "$PYPI_SITE/current" ] || fail "pypi current link not restored"

PYPI_SERVER_LOG="$WORK/pypi-server.log"
"$STUFF_SERVER" --bind "127.0.0.1:$PYPI_PORT" --route "/=$PYPI_SITE" >"$PYPI_SERVER_LOG" 2>&1 &
PYPI_SERVER_PID=$!

for _ in $(seq 50); do
  grep -q "listening on" "$PYPI_SERVER_LOG" 2>/dev/null && break
  sleep 0.1
done
grep -q "listening on" "$PYPI_SERVER_LOG" || fail "stuff-server for pypi did not start"

PYPI_VENV="$WORK/pypi-venv"
uv venv -q --python 3.15 "$PYPI_VENV" || fail "uv venv failed"
VIRTUAL_ENV="$PYPI_VENV" uv pip install -q --python 3.15 --index-url "http://127.0.0.1:$PYPI_PORT/simple/" --only-binary :all: "six==1.16.0" || fail "uv pip install from stuff-server failed"
VIRTUAL_ENV="$PYPI_VENV" uv run -q --no-project --python 3.15 python -c "import six; assert six.__version__ == '1.16.0', six.__version__" || fail "installed six did not import cleanly"
echo "ok: pypi publish, pull, serve, and uv install"

# 3. NPM: publish tarball with stuff, pull with r2-pull.py, serve, install with npm
echo "==> Testing npm publish and real npm install"
NPM_OUT=$("$STUFF" publish npm --tarball "$ROOT/tests/fixtures/mynpm/tarballs/flatdemo-1.2.3.tgz") || fail "stuff publish npm failed"
echo "$NPM_OUT" | grep -q "npm published generation" || fail "unexpected output: $NPM_OUT"

NPM_SITE="$WORK/npm-site"
python3 "$ROOT/deploy/r2-pull.py" test-npm "$NPM_SITE" || fail "r2-pull.py failed for npm"
[ -d "$NPM_SITE/current" ] || fail "npm current link not restored"

NPM_SERVER_LOG="$WORK/npm-server.log"
"$STUFF_SERVER" --bind "127.0.0.1:$NPM_PORT" --route "/=$NPM_SITE" >"$NPM_SERVER_LOG" 2>&1 &
NPM_SERVER_PID=$!

for _ in $(seq 50); do
  grep -q "listening on" "$NPM_SERVER_LOG" 2>/dev/null && break
  sleep 0.1
done
grep -q "listening on" "$NPM_SERVER_LOG" || fail "stuff-server for npm did not start"

NPM_CLIENT_DIR="$WORK/npm-client"
mkdir -p "$NPM_CLIENT_DIR"
cat > "$NPM_CLIENT_DIR/package.json" <<'EOF'
{
  "name": "test-consumer",
  "version": "1.0.0"
}
EOF

(cd "$NPM_CLIENT_DIR" && npm --registry "http://127.0.0.1:$NPM_PORT/current" install --no-audit --no-fund "flatdemo@1.2.3") || fail "npm install from stuff-server failed"
(cd "$NPM_CLIENT_DIR" && node -e "const pkg = require('flatdemo'); if (!pkg) process.exit(1);") || fail "installed flatdemo failed to require"
echo "ok: npm publish, pull, serve, and npm install"

# 4. OCI: publish layout with stuff, pull verification
echo "==> Testing OCI publish"
OCI_OUT=$("$STUFF" publish oci --layout "$ROOT/tests/fixtures/oci/layout" --repository example/app) || fail "stuff publish oci failed"
echo "$OCI_OUT" | grep -q "oci published generation" || fail "unexpected output: $OCI_OUT"
echo "ok: oci publish"

# 5. CLI commands: ls, diff, doctor against pulled sites
echo "==> Testing stuff ls and stuff doctor against pulled publications"
"$STUFF" ls --pypi-dir "$PYPI_SITE" | grep -q "six" || fail "stuff ls did not find six"
"$STUFF" ls --npm-dir "$NPM_SITE" | grep -q "flatdemo" || fail "stuff ls did not find flatdemo"
"$STUFF" doctor --pypi-dir "$PYPI_SITE" --pypi-base "http://127.0.0.1:$PYPI_PORT" >/dev/null || fail "stuff doctor failed on pypi"
"$STUFF" doctor --npm-dir "$NPM_SITE" --npm-base "http://127.0.0.1:$NPM_PORT" >/dev/null || fail "stuff doctor failed on npm"
echo "ok: stuff ls and stuff doctor"

echo "PASS: stuff clean-machine e2e integration acceptance"
