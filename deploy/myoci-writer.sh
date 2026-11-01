#!/bin/sh
# Run the long-lived OCI writer on the push host.
# Set STUFF_OCI_WRITER_ORIGIN on the Worker to this host's public URL.
# Env: MYOCI_WRITER_BIND (0.0.0.0:5000), MYOCI_WRITER_STATE,
#      MYOCI_WRITER_SECRET_FILE (/run/secrets/oci-push-secret),
#      MYOCI_WRITER_BUCKET (stuff-oci-blobs), MYOCI_WRITER_LOCAL=1 for a
#      local sink. The writer loads R2 S3 credentials from R2_S3_*.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIND=${MYOCI_WRITER_BIND:-0.0.0.0:5000}
STATE_DIR=${MYOCI_WRITER_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/stuff/oci/writer}
SECRET_FILE=${MYOCI_WRITER_SECRET_FILE:-/run/secrets/oci-push-secret}
BUCKET=${MYOCI_WRITER_BUCKET:-stuff-oci-blobs}
DEPLOY_DIR=${MYOCI_WRITER_DEPLOY_DIR:-$ROOT/deploy}
[ -r "$SECRET_FILE" ] || { echo "myoci-writer: secret file is not readable: $SECRET_FILE" >&2; exit 1; }
(cd "$ROOT" && cargo build -q -p myoci)
if [ -n "${MYOCI_WRITER_LOCAL:-}" ]; then
  exec "$ROOT/target/debug/myoci" writer --state-dir "$STATE_DIR" --bind "$BIND" --secret-file "$SECRET_FILE" \
    --public-origin "${MYOCI_WRITER_PUBLIC_ORIGIN:-https://oci.17166969.xyz}"
fi
exec "$ROOT/target/debug/myoci" writer --state-dir "$STATE_DIR" --bind "$BIND" \
  --secret-file "$SECRET_FILE" --r2-bucket "$BUCKET" --deploy-dir "$DEPLOY_DIR" \
  --public-origin "${MYOCI_WRITER_PUBLIC_ORIGIN:-https://oci.17166969.xyz}"
