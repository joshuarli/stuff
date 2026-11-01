#!/bin/sh
# Prove per-registry publication pipelines never touch each other's output.
# Publishes mycrates (offline), mypypi (one tiny wheel), and mynpm (empty
# inventory, offline) through the Rust binaries and asserts mutual
# non-interference plus publish determinism.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-"$ROOT/.work/pipecheck"}
rm -rf "$WORK"
mkdir -p "$WORK"

CRATES_STATE="$WORK/crates-state"
CRATES_SITE="$WORK/crates-site"
PYPI_STATE="$WORK/pypi-state"
PYPI_OUT="$WORK/pypi-site"
NPM_STATE="$WORK/npm-state"
NPM_SITE="$WORK/npm-site"
OCI_STATE="$WORK/oci-state"
OCI_SITE="$WORK/oci-site"

if command -v sha256sum >/dev/null 2>&1; then
  hash_file() { sha256sum "$1" | cut -d' ' -f1; }
else
  hash_file() { shasum -a 256 "$1" | cut -d' ' -f1; }
fi

snapshot() {
  out=$1
  only=${2:-}
  : > "$out"
  for d in "$CRATES_SITE" "$PYPI_OUT" "$NPM_SITE" "$OCI_SITE"; do
    [ -e "$d" ] || continue
    case $d in
      *"$only"*) ;;
      *) [ -n "$only" ] && continue ;;
    esac
    find "$d" | sort | while IFS= read -r f; do
      if [ -L "$f" ]; then
        echo "L ${f#$WORK/} -> $(readlink "$f")"
      elif [ -f "$f" ]; then
        echo "F ${f#$WORK/} $(hash_file "$f")"
      fi
    done >> "$out"
  done
}

(cd "$ROOT" && cargo run -q -p mycrates -- init --state-dir "$CRATES_STATE" --site-dir "$CRATES_SITE" >/dev/null)
(cd "$ROOT" && cargo run -q -p mycrates -- publish --state-dir "$CRATES_STATE" --site-dir "$CRATES_SITE" >/dev/null)
snapshot "$WORK/after-crates.txt" "crates-site"
CRATES_GEN_1=$(readlink "$CRATES_SITE/current")

UNAME_S=$(uname -s)
UNAME_M=$(uname -m)
case "$UNAME_S-$UNAME_M" in
  Linux-x86_64) HOST_PLATFORM="linux-x86_64" ;;
  Linux-aarch64|Linux-arm64) HOST_PLATFORM="linux-aarch64" ;;
  Darwin-arm64) HOST_PLATFORM="macos-arm64" ;;
  Darwin-x86_64) HOST_PLATFORM="macos-x86_64" ;;
  *) echo "FAIL: unsupported host platform $UNAME_S-$UNAME_M"; exit 1 ;;
esac

cat > "$WORK/pyproject.toml" <<EOF
[project]
name = "pipecheck"
version = "0.1.0"
requires-python = ">=3.15,<3.16"

[tool.mypypi.targets]
platforms = ["linux-x86_64", "linux-aarch64", "macos-x86_64", "macos-arm64"]
linux_repair_policy = "manylinux_2_28"
EOF
cat > "$WORK/packages.toml" <<'EOF'
[[packages]]
name = "six"
version = "1.16.0"
EOF
(cd "$ROOT" && cargo run -q -p mypypi -- --catalog "$WORK/packages.toml" --config "$WORK/pyproject.toml" --state-dir "$PYPI_STATE" add "six==1.16.0" >/dev/null)
(cd "$ROOT" && cargo run -q -p mypypi -- --catalog "$WORK/packages.toml" --config "$WORK/pyproject.toml" --state-dir "$PYPI_STATE" --output "$PYPI_OUT" build >/dev/null)
(cd "$ROOT" && cargo run -q -p mypypi -- --catalog "$WORK/packages.toml" --config "$WORK/pyproject.toml" --state-dir "$PYPI_STATE" --output "$PYPI_OUT" --platform "$HOST_PLATFORM" validate >/dev/null)
(cd "$ROOT" && cargo run -q -p mypypi -- --catalog "$WORK/packages.toml" --config "$WORK/pyproject.toml" --state-dir "$PYPI_STATE" --output "$PYPI_OUT" --platform "$HOST_PLATFORM" publish >/dev/null)
snapshot "$WORK/after-pypi.txt" "pypi-site"
if cmp -s "$WORK/after-crates.txt" "$WORK/after-pypi-crates.txt" 2>/dev/null; then :; fi
snapshot "$WORK/after-pypi-crates.txt" "crates-site"
if cmp -s "$WORK/after-crates.txt" "$WORK/after-pypi-crates.txt"; then
  echo "crosstalk: pypi publish left crates-site untouched"
else
  echo "FAIL: pypi publish changed crates-site"; exit 1
fi

(cd "$ROOT" && cargo run -q -p mycrates -- publish --state-dir "$CRATES_STATE" --site-dir "$CRATES_SITE" >/dev/null)
snapshot "$WORK/after-crates-again.txt" "crates-site"
CRATES_GEN_2=$(readlink "$CRATES_SITE/current")
[ "$CRATES_GEN_1" = "$CRATES_GEN_2" ] || { echo "FAIL: crates republish changed generation"; exit 1; }
if cmp -s "$WORK/after-crates.txt" "$WORK/after-crates-again.txt"; then
  echo "determinism: crates republish is a noop"
else
  echo "FAIL: crates republish changed output"; exit 1
fi
PYPI_GEN_1=$(readlink "$PYPI_OUT/current")
(cd "$ROOT" && cargo run -q -p mypypi -- --catalog "$WORK/packages.toml" --config "$WORK/pyproject.toml" --state-dir "$PYPI_STATE" --output "$PYPI_OUT" --platform "$HOST_PLATFORM" publish >/dev/null)
PYPI_GEN_2=$(readlink "$PYPI_OUT/current")
[ "$PYPI_GEN_1" = "$PYPI_GEN_2" ] || { echo "FAIL: pypi republish changed generation"; exit 1; }
snapshot "$WORK/after-pypi-again.txt" "pypi-site"
if cmp -s "$WORK/after-pypi.txt" "$WORK/after-pypi-again.txt"; then
  echo "determinism: pypi republish is a noop"
else
  echo "FAIL: pypi republish changed output"; exit 1
fi

(cd "$WORK" && cargo run -q -p mynpm -- init-config --node-version 26.12.1 --public-url https://npm.17166969.xyz --output "$WORK/mynpm.json" >/dev/null)
(cd "$ROOT" && cargo run -q -p mynpm -- --config "$WORK/mynpm.json" --state-dir "$NPM_STATE" publish --site-dir "$NPM_SITE" >/dev/null)
snapshot "$WORK/after-npm-crates.txt" "crates-site"
if cmp -s "$WORK/after-crates-again.txt" "$WORK/after-npm-crates.txt"; then
  echo "crosstalk: npm publish left crates-site untouched"
else
  echo "FAIL: npm publish changed crates-site"; exit 1
fi
snapshot "$WORK/after-npm-pypi.txt" "pypi-site"
if cmp -s "$WORK/after-pypi-again.txt" "$WORK/after-npm-pypi.txt"; then
  echo "crosstalk: npm publish left pypi-site untouched"
else
  echo "FAIL: npm publish changed pypi-site"; exit 1
fi
snapshot "$WORK/after-npm.txt" "npm-site"
NPM_GEN_1=$(readlink "$NPM_SITE/mirror/current")
(cd "$ROOT" && cargo run -q -p mynpm -- --config "$WORK/mynpm.json" --state-dir "$NPM_STATE" publish --site-dir "$NPM_SITE" >/dev/null)
snapshot "$WORK/after-npm-again.txt" "npm-site"
NPM_GEN_2=$(readlink "$NPM_SITE/mirror/current")
[ "$NPM_GEN_1" = "$NPM_GEN_2" ] || { echo "FAIL: npm republish changed generation"; exit 1; }
if cmp -s "$WORK/after-npm.txt" "$WORK/after-npm-again.txt"; then
  echo "determinism: npm republish is a noop"
else
  echo "FAIL: npm republish changed output"; exit 1
fi

snapshot "$WORK/before-oci-crates.txt" "crates-site"
snapshot "$WORK/before-oci-pypi.txt" "pypi-site"
snapshot "$WORK/before-oci-npm.txt" "npm-site"
(cd "$ROOT" && cargo run -q -p myoci -- init --state-dir "$OCI_STATE" >/dev/null)
(cd "$ROOT" && cargo run -q -p myoci -- import --state-dir "$OCI_STATE" --layout tests/fixtures/oci/layout --repository example/app >/dev/null)
(cd "$ROOT" && cargo run -q -p myoci -- publish --state-dir "$OCI_STATE" --site-dir "$OCI_SITE" >/dev/null)
snapshot "$WORK/after-oci-crates.txt" "crates-site"
snapshot "$WORK/after-oci-pypi.txt" "pypi-site"
snapshot "$WORK/after-oci-npm.txt" "npm-site"
if cmp -s "$WORK/before-oci-crates.txt" "$WORK/after-oci-crates.txt" \
  && cmp -s "$WORK/before-oci-pypi.txt" "$WORK/after-oci-pypi.txt" \
  && cmp -s "$WORK/before-oci-npm.txt" "$WORK/after-oci-npm.txt"; then
  echo "crosstalk: oci publish left the other registries untouched"
else
  echo "FAIL: oci publish changed another registry"; exit 1
fi
OCI_GEN_1=$(readlink "$OCI_SITE/current")
(cd "$ROOT" && cargo run -q -p myoci -- publish --state-dir "$OCI_STATE" --site-dir "$OCI_SITE" >/dev/null)
OCI_GEN_2=$(readlink "$OCI_SITE/current")
[ "$OCI_GEN_1" = "$OCI_GEN_2" ] || { echo "FAIL: oci republish changed generation"; exit 1; }
snapshot "$WORK/after-oci-site.txt" "oci-site"
(cd "$ROOT" && cargo run -q -p myoci -- publish --state-dir "$OCI_STATE" --site-dir "$OCI_SITE" >/dev/null)
snapshot "$WORK/after-oci-again.txt" "oci-site"
if cmp -s "$WORK/after-oci-site.txt" "$WORK/after-oci-again.txt"; then
  echo "determinism: oci republish is a noop"
else
  echo "FAIL: oci republish changed output"; exit 1
fi
echo "OK: single-registry publishes leave other generations untouched"
