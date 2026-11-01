#!/bin/sh
# Publish the OCI registry: import images into local state, render a
# generation, and sync blobs before metadata. Called by deploy/publish oci.
#
# Inputs (unless REPUBLISH=1):
#   OCI_IMAGES="repository=reference ..."  import each reference through oras
#   OCI_LAYOUT=DIR OCI_REPOSITORY=NAME [OCI_TAG=TAG]  import a local layout
# Optional env (defaults shown):
#   STUFF_DIR=.. PUBLIC_URL=https://oci.17166969.xyz
#   BUCKET_META=stuff-oci BUCKET_BLOBS=stuff-oci-blobs
#   STUFF_STATE_HOME=${XDG_STATE_HOME:-$HOME/.local/state}/stuff
#   STATE_DIR=$STUFF_STATE_HOME/oci/state SITE_DIR=$STUFF_STATE_HOME/oci/site
#   LOCK_DIR=$STUFF_STATE_HOME/oci/publish.lock
set -eu
STUFF_DIR=${STUFF_DIR:-../stuff}
PUBLIC_URL=${PUBLIC_URL:-https://oci.17166969.xyz}
BUCKET_META=${BUCKET_META:-stuff-oci}
BUCKET_BLOBS=${BUCKET_BLOBS:-stuff-oci-blobs}
STUFF_STATE_HOME=${STUFF_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/stuff}
STATE_DIR=${STATE_DIR:-$STUFF_STATE_HOME/oci/state}
SITE_DIR=${SITE_DIR:-$STUFF_STATE_HOME/oci/site}
LOCK_DIR=${LOCK_DIR:-$STUFF_STATE_HOME/oci/publish.lock}
TMPDIR=${TMPDIR:-"$STUFF_DIR/.work/tmp"}
export TMPDIR
mkdir -p "$TMPDIR"

. "$STUFF_DIR/deploy/publish-lock.sh"
mkdir -p "$STUFF_STATE_HOME/oci"
acquire_publish_lock "$LOCK_DIR"
META=""
cleanup() {
  status=$?
  trap - EXIT
  [ -z "$META" ] || rm -rf "$META" || true
  release_publish_lock || true
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

MYOCI=$STUFF_DIR/target/debug/myoci
R2_SYNC=$STUFF_DIR/deploy/r2-sync.sh
R2_SYNC_BLOBS=$STUFF_DIR/deploy/r2-sync-blobs.sh
PRUNE=$STUFF_DIR/deploy/prune-generations.sh

cargo build -q -p myoci --manifest-path "$STUFF_DIR/Cargo.toml"

if [ -n "${REPUBLISH:-}" ]; then
  :
else
  [ -n "${OCI_IMAGES:-}" ] || [ -n "${OCI_LAYOUT:-}" ] || {
    echo "publish-myoci: OCI_IMAGES or OCI_LAYOUT is empty" >&2
    exit 1
  }
  "$MYOCI" init --state-dir "$STATE_DIR" >/dev/null
  if [ -n "${OCI_LAYOUT:-}" ]; then
    [ -n "${OCI_REPOSITORY:-}" ] || {
      echo "publish-myoci: OCI_REPOSITORY is required with OCI_LAYOUT" >&2
      exit 1
    }
    if [ -n "${OCI_TAG:-}" ]; then
      "$MYOCI" import --state-dir "$STATE_DIR" --layout "$OCI_LAYOUT" \
        --repository "$OCI_REPOSITORY" --tag "$OCI_TAG"
    else
      "$MYOCI" import --state-dir "$STATE_DIR" --layout "$OCI_LAYOUT" \
        --repository "$OCI_REPOSITORY"
    fi
  fi
  for image in ${OCI_IMAGES:-}; do
    repository=${image%%=*}
    reference=${image#*=}
    [ "$repository" != "$image" ] || {
      echo "publish-myoci: OCI_IMAGES entries are repository=reference: $image" >&2
      exit 1
    }
    "$MYOCI" import --state-dir "$STATE_DIR" --from "$reference" --repository "$repository"
  done
fi

"$MYOCI" verify --state-dir "$STATE_DIR"
"$MYOCI" publish --state-dir "$STATE_DIR" --site-dir "$SITE_DIR" --public-url "$PUBLIC_URL"

META=$(mktemp -d "$STUFF_STATE_HOME/oci/meta-sync.XXXXXX")
(cd "$SITE_DIR" && tar -cf - --exclude=./blobs .) | (cd "$META" && tar -xf -)
"$R2_SYNC_BLOBS" "$SITE_DIR" "$BUCKET_BLOBS"
"$R2_SYNC" "$META" "$BUCKET_META"
"$PRUNE" "$SITE_DIR"
