#!/bin/sh
# Run the OCI writer from the repo's pinned Linux build image.
# Env: MYOCI_WRITER_PORT (5000), MYOCI_WRITER_IMAGE (stuff-myoci-writer:2026-11-01-manylinux_2_28),
#      MYOCI_WRITER_SECRET (host path to the shared HMAC secret),
#      MYOCI_WRITER_STATE (host state dir), R2_S3_* (passed through).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PORT=${MYOCI_WRITER_PORT:-5000}
IMAGE=${MYOCI_WRITER_IMAGE:-stuff-myoci-writer:2026-11-01-manylinux_2_28}
SECRET=${MYOCI_WRITER_SECRET:-$ROOT/.work/oci-writer-secret}
STATE=${MYOCI_WRITER_STATE:-$ROOT/.work/oci-writer-state}
case "$(uname -m)" in
  arm64|aarch64) ARCH=arm64 ;;
  x86_64|amd64) ARCH=amd64 ;;
  *) echo "myoci-writer-container: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
BASE_IMAGE=$(tr -d '[:space:]' < "$ROOT/deploy/manylinux_2_28-$ARCH")
[ -r "$SECRET" ] || { echo "myoci-writer-container: secret is not readable: $SECRET" >&2; exit 1; }
mkdir -p "$STATE"
if [ -n "${IMAGE_REBUILD:-}" ] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  docker build --platform "linux/$ARCH" --build-arg MANYLINUX_IMAGE="$BASE_IMAGE" -t "$IMAGE" -f "$ROOT/deploy/mypypi-linux.Dockerfile" "$ROOT/deploy"
fi
docker run --rm --platform "linux/$ARCH" --name stuff-myoci-writer -p "$PORT:5000" \
  -v "$ROOT:/work" -v "$SECRET:/run/secrets/oci-push-secret:ro" -v "$STATE:/state" \
  -w /work -e RUSTUP_TOOLCHAIN=nightly-2026-09-15 \
  -e "CARGO_HOME=/work/.work/linux/cargo-home-writer" \
  -e "RUSTUP_HOME=/work/.work/linux/rustup-home-writer" \
  -e "CARGO_TARGET_DIR=/work/.work/linux/target-writer" \
  -e STUFF_OCI_PUSH_SECRET_FILE=/run/secrets/oci-push-secret \
  -e "MYOCI_WRITER_STATE=/state" -e MYOCI_WRITER_BIND=0.0.0.0:5000 \
  -e "MYOCI_WRITER_PUBLIC_ORIGIN=${MYOCI_WRITER_PUBLIC_ORIGIN:-https://oci.17166969.xyz}" \
  -e R2_S3_ENDPOINT -e R2_S3_ACCESS_KEY_ID -e R2_S3_SECRET_ACCESS_KEY \
  -e R2_S3_SESSION_TOKEN -e R2_S3_REGION \
  "$IMAGE" sh -c 'mkdir -p "$RUSTUP_HOME/toolchains"; toolchain=$(basename "$(ls -d /opt/rustup/toolchains/* | head -1)"); ln -sfn "/opt/rustup/toolchains/$toolchain" "$RUSTUP_HOME/toolchains/$toolchain"; cp -f /opt/rustup/settings.toml "$RUSTUP_HOME/settings.toml"; exec deploy/myoci-writer.sh'
