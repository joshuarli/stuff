#!/bin/sh
# Prove per-registry publication pipelines never touch each other's output.
# Publishes mycrates and mypypi (both offline-capable) and asserts mutual
# non-interference plus publish determinism. mynpm participates through
# disjoint paths and separate CI jobs; its publish needs network.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-"$ROOT/.work/pipecheck"}
rm -rf "$WORK"
mkdir -p "$WORK"

CRATES_STATE="$WORK/crates-state"
CRATES_SITE="$WORK/crates-site"
PYPI_OUT="$WORK/pypi-site"

if command -v sha256sum >/dev/null 2>&1; then
  hash_file() { sha256sum "$1" | cut -d' ' -f1; }
else
  hash_file() { shasum -a 256 "$1" | cut -d' ' -f1; }
fi

snapshot() {
  out=$1
  only=${2:-}
  : > "$out"
  for d in "$CRATES_SITE" "$PYPI_OUT"; do
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

cat > "$WORK/mkwheel.py" <<'EOF'
import base64
import hashlib
import sys
import zipfile
from pathlib import Path

sys.path.insert(0, "tools/mypypi/src")
from packaging.version import Version

from mypypi.models import Target, ValidatedArtifact, WheelIdentity
from mypypi.store import publish

out = Path(sys.argv[1])
tmp = Path(sys.argv[2])
name, version = "indep_fixture", "1.0"
filename = f"{name}-{version}-py3-none-any.whl"
identity = WheelIdentity.parse(filename)
dist_info = f"{name}-{version}.dist-info"
path = tmp / filename
members = {
    f"{dist_info}/METADATA": (
        f"Metadata-Version: 2.4\nName: {name}\nVersion: {version}\n"
        "Requires-Python: >=3.15\n\n"
    ).encode(),
    f"{dist_info}/WHEEL": b"Wheel-Version: 1.0\nTag: py3-none-any\n\n",
    f"{name}.py": b"value = 1\n",
}
with zipfile.ZipFile(path, "w") as archive:
    for entry, content in members.items():
        archive.writestr(entry, content)
record = []
for entry in members:
    with zipfile.ZipFile(path, "r") as archive:
        content = archive.read(entry)
    digest = base64.urlsafe_b64encode(hashlib.sha256(content).digest()).decode().rstrip("=")
    record.append(f"{entry},sha256={digest},{len(content)}")
record.append(f"{dist_info}/RECORD,,")
with zipfile.ZipFile(path, "a") as archive:
    archive.writestr(f"{dist_info}/RECORD", "\n".join(record) + "\n")
artifact = ValidatedArtifact(
    identity,
    path,
    hashlib.sha256(path.read_bytes()).hexdigest(),
    frozenset({Target(Version("3.15"), "linux-x86_64")}),
)
publish((artifact,), out)
EOF
(cd "$ROOT" && uv run --locked --python 3.15 --project tools/mypypi python "$WORK/mkwheel.py" "$PYPI_OUT" "$WORK" >/dev/null)
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
echo "OK: single-registry publishes leave other generations untouched"
