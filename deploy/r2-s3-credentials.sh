#!/bin/sh
# Source this to get s3_credentials_set and mint_s3_credentials. The caller
# sets SCRIPT_DIR to the deploy directory.

s3_credentials_set() {
  [ -n "${R2_S3_ENDPOINT:-}" ] && [ -n "${R2_S3_ACCESS_KEY_ID:-}" ] && [ -n "${R2_S3_SECRET_ACCESS_KEY:-}" ]
}

mint_s3_credentials() {
  if s3_credentials_set; then
    return 0
  fi
  if minted=$(python3 "$SCRIPT_DIR/r2-temp-credentials.py" "$1" 2>&1); then
    eval "$minted"
    return 0
  fi
  printf '%s\n' "$minted" | sed 's/^/note: /' >&2
  return 1
}
