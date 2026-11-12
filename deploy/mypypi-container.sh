#!/bin/sh
# Run mypypi for one Linux platform inside a container whose architecture
# matches the target, against the same catalog, state, and site as the host.
#
# usage: mypypi-container.sh ARCH(arm64|amd64) PLATFORM(linux-aarch64|linux-x86_64) COMMAND [ARGS...]
# Optional env: STUFF_STATE_HOME STATE_DIR SITE_DIR CATALOG_FILE CONFIG_FILE
# STUFF (the stuff CLI), IMAGE_REBUILD=1.
# Holds the pypi registry's publish lock (`stuff lock-path --registry pypi`)
# for the whole container run, so platforms and `stuff publish pypi` write
# one at a time.
# The image is built from deploy/mypypi-linux.Dockerfile. The container is
# started detached and waited on; `docker run --rm` and `docker rm` can hang
# on OrbStack after the container has exited.
set -eu
ARCH=$1; PLATFORM=$2; shift 2
ROOT=$(cd "$(dirname "$0")/.." && pwd)
STUFF_STATE_HOME=${STUFF_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/stuff}
STATE_DIR=${STATE_DIR:-$STUFF_STATE_HOME/mypypi/state}
SITE_DIR=${SITE_DIR:-$STUFF_STATE_HOME/mypypi/site}
CATALOG_FILE=${CATALOG_FILE:-$STUFF_STATE_HOME/mypypi/packages.toml}
CONFIG_FILE=${CONFIG_FILE:-$STUFF_STATE_HOME/mypypi/pyproject.toml}
IMAGE=stuff-mypypi:manylinux_2_28-$ARCH
BASE_IMAGE=$(tr -d '[:space:]' < "$ROOT/deploy/manylinux_2_28-$ARCH")
absolute_path() {
  case "$1" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s\n' "$ROOT/$1" ;;
  esac
}
STATE_DIR=$(absolute_path "$STATE_DIR")
SITE_DIR=$(absolute_path "$SITE_DIR")
CATALOG_FILE=$(absolute_path "$CATALOG_FILE")
CONFIG_FILE=$(absolute_path "$CONFIG_FILE")
CATALOG_DIR=$(dirname "$CATALOG_FILE")
CONFIG_DIR=$(dirname "$CONFIG_FILE")
mkdir -p "$STATE_DIR" "$SITE_DIR" "$CATALOG_DIR" "$CONFIG_DIR"

. "$ROOT/deploy/publish-lock.sh"
acquire_registry_lock pypi
trap 'release_publish_lock' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

CANDIDATES=$STATE_DIR
for path in "$SITE_DIR" "$CATALOG_DIR" "$CONFIG_DIR"; do
  case " $CANDIDATES " in
    *" $path "*) ;;
    *) CANDIDATES="$CANDIDATES $path" ;;
  esac
done
MOUNT_PATHS=
for path in $CANDIDATES; do
  covered=
  for other in $CANDIDATES; do
    [ "$path" = "$other" ] && continue
    case "$path/" in
      "$other/"*) covered=1 ;;
    esac
  done
  [ -n "$covered" ] || MOUNT_PATHS="$MOUNT_PATHS $path"
done
MOUNTS=
for path in $MOUNT_PATHS; do
  MOUNTS="$MOUNTS -v $path:$path"
done

if [ -n "${IMAGE_REBUILD:-}" ] || ! docker image inspect "$IMAGE" > /dev/null 2>&1; then
  docker build --platform "linux/$ARCH" --build-arg MANYLINUX_IMAGE="$BASE_IMAGE" -t "$IMAGE" -f "$ROOT/deploy/mypypi-linux.Dockerfile" "$ROOT/deploy"
fi
NAME=stuff-mypypi-$ARCH-$$
HOME_DIR=$ROOT/.work/tmp/mypypi-home-$ARCH
mkdir -p "$HOME_DIR"
CONTAINER_CMD="mkdir -p \"\$RUSTUP_HOME/toolchains\"; toolchain=\$(basename \"\$(ls -d /opt/rustup/toolchains/* | head -1)\"); ln -sfn \"/opt/rustup/toolchains/\$toolchain\" \"\$RUSTUP_HOME/toolchains/\$toolchain\"; cp -f /opt/rustup/settings.toml \"\$RUSTUP_HOME/settings.toml\"; cargo build -q -p mypypi && .work/linux/target-$ARCH/debug/mypypi --catalog $CATALOG_FILE --config $CONFIG_FILE --state-dir $STATE_DIR --output $SITE_DIR --platform $PLATFORM $*"
# shellcheck disable=SC2086
docker run -d --init --name "$NAME" --platform "linux/$ARCH" --user "$(id -u):$(id -g)" \
  -v "$ROOT:/work" $MOUNTS -w /work \
  -e "HOME=$HOME_DIR" -e RUSTUP_TOOLCHAIN=nightly-2026-09-15 \
  -e "CARGO_HOME=/work/.work/linux/cargo-home-$ARCH" \
  -e "RUSTUP_HOME=/work/.work/linux/rustup-home-$ARCH" \
  -e "CARGO_TARGET_DIR=/work/.work/linux/target-$ARCH" \
  -e TMPDIR=/work/.work/tmp "$IMAGE" sh -c "$CONTAINER_CMD" > /dev/null
docker logs -f "$NAME" 2>&1 &
LOGS=$!
STATUS=$(docker wait "$NAME")
wait "$LOGS" || true
(docker rm -f "$NAME" > /dev/null 2>&1 &)
exit "$STATUS"
