#!/bin/sh
# Run mypypi for one Linux platform inside a container whose architecture
# matches the target, against the same catalog, state, and site as the host.
#
# usage: mypypi-container.sh ARCH(arm64|amd64) PLATFORM(linux-aarch64|linux-x86_64) COMMAND [ARGS...]
# Optional env: STATE_DIR SITE_DIR CATALOG_FILE CONFIG_FILE (paths under the
# repo root, defaults as in publish-mypypi.sh), IMAGE_REBUILD=1.
# The image is built from deploy/mypypi-linux.Dockerfile. The container is
# started detached and waited on; `docker run --rm` and `docker rm` can hang
# on OrbStack after the container has exited.
set -eu
ARCH=$1; PLATFORM=$2; shift 2
ROOT=$(cd "$(dirname "$0")/.." && pwd)
STATE_DIR=${STATE_DIR:-.work/registry-pub/state}
SITE_DIR=${SITE_DIR:-.work/registry-pub/site}
CATALOG_FILE=${CATALOG_FILE:-.work/registry-pub/packages.toml}
CONFIG_FILE=${CONFIG_FILE:-.work/registry-pub/pyproject.toml}
IMAGE=stuff-mypypi:$ARCH

if [ -n "${IMAGE_REBUILD:-}" ] || ! docker image inspect "$IMAGE" > /dev/null 2>&1; then
  docker build --platform "linux/$ARCH" -t "$IMAGE" -f "$ROOT/deploy/mypypi-linux.Dockerfile" "$ROOT/deploy"
fi
NAME=stuff-mypypi-$ARCH-$$
docker run -d --name "$NAME" --platform "linux/$ARCH" -v "$ROOT:/work" -w /work \
  -e RUSTUP_TOOLCHAIN=nightly-2026-09-15 -e "CARGO_TARGET_DIR=/work/.work/linux/target-$ARCH" \
  -e TMPDIR=/work/.work/tmp "$IMAGE" sh -c "cargo build -q -p mypypi && \
  .work/linux/target-$ARCH/debug/mypypi --catalog $CATALOG_FILE --config $CONFIG_FILE \
  --state-dir $STATE_DIR --output $SITE_DIR --platform $PLATFORM $*" > /dev/null
docker logs -f "$NAME" 2>&1 &
LOGS=$!
STATUS=$(docker wait "$NAME")
wait "$LOGS" || true
(docker rm -f "$NAME" > /dev/null 2>&1 &)
exit "$STATUS"
