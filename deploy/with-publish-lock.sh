#!/bin/sh
# Run a command under the publish lock of the registry that owns BUCKET.
# Usage: with-publish-lock.sh BUCKET COMMAND [ARG...]
set -eu
[ $# -ge 2 ] || { echo "usage: with-publish-lock.sh BUCKET COMMAND [ARG...]" >&2; exit 2; }
BUCKET=$1
shift
. "$(dirname "$0")/publish-lock.sh"
acquire_publish_lock "$(bucket_lock_path "$BUCKET")"
exec "$@"
