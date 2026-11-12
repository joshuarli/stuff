#!/bin/sh
# Run a command against the workspace inside the Linux build image.
# usage: test-linux.sh COMMAND [ARGS...]
# Optional env: ARCH(arm64|amd64) IMAGE_REBUILD=1
set -eu
if [ "$#" -eq 0 ]; then
  echo "usage: test-linux.sh COMMAND [ARGS...]" >&2
  exit 2
fi
ROOT=$(cd "$(dirname "$0")/.." && pwd)
case "${ARCH:-$(uname -m)}" in
  arm64|aarch64) ARCH=arm64 ;;
  x86_64|amd64) ARCH=amd64 ;;
  *) echo "FAIL: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
IMAGE=stuff-linux:manylinux_2_28-$ARCH
BASE_IMAGE=$(tr -d '[:space:]' < "$ROOT/deploy/manylinux_2_28-$ARCH")
if [ -n "${IMAGE_REBUILD:-}" ] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  docker build --platform "linux/$ARCH" --build-arg MANYLINUX_IMAGE="$BASE_IMAGE" -t "$IMAGE" -f "$ROOT/deploy/mypypi-linux.Dockerfile" "$ROOT/deploy"
fi
WORK=$ROOT/.work/linux
CONTAINER_WORK=/work/.work/linux
mkdir -p "$WORK/tmp" "$WORK/home-$ARCH" "$WORK/cargo-home-$ARCH" "$WORK/rustup-home-$ARCH" "$WORK/target-$ARCH"
NAME=stuff-linux-$ARCH-$$
cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# shellcheck disable=SC2016
docker run -d --init --name "$NAME" --platform "linux/$ARCH" --user "$(id -u):$(id -g)" \
  -v "$ROOT:/work" -w /work \
  -e "HOME=$CONTAINER_WORK/home-$ARCH" \
  -e "CARGO_HOME=$CONTAINER_WORK/cargo-home-$ARCH" \
  -e "RUSTUP_HOME=$CONTAINER_WORK/rustup-home-$ARCH" \
  -e "CARGO_TARGET_DIR=$CONTAINER_WORK/target-$ARCH" \
  -e "TMPDIR=$CONTAINER_WORK/tmp" \
  -e "RUSTUP_TOOLCHAIN=nightly-2026-09-15" \
  "$IMAGE" sh -c 'mkdir -p "$RUSTUP_HOME/toolchains"; toolchain=$(basename "$(ls -d /opt/rustup/toolchains/* | head -1)"); ln -sfn "/opt/rustup/toolchains/$toolchain" "$RUSTUP_HOME/toolchains/$toolchain"; cp -f /opt/rustup/settings.toml "$RUSTUP_HOME/settings.toml"; exec "$@"' sh "$@" >/dev/null
STATUS=$(docker wait "$NAME")
docker logs "$NAME" 2>&1
docker rm -f "$NAME" >/dev/null 2>&1 || true
trap - EXIT
exit "$STATUS"
