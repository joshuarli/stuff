#!/bin/sh
set -eu
host=${1:?usage: crates/stuff-worker/orange/build-image.sh HOST}
root=$(git rev-parse --show-toplevel)
cd "$root"
test -z "$(git status --porcelain --untracked-files=normal)" || { echo "orange: commit changes first, so the image tag names its contents" >&2; exit 1; }
release=$(git rev-parse --short=12 HEAD)
context="$root/.work/tmp/stuff-orange-image-$release"
rm -rf "$context"
mkdir -p "$context/orange"
(cd crates/stuff-worker && pnpm run build) >&2
node crates/stuff-worker/scripts/stage-orange.mjs "$context/app" "stuff-$release" >&2
cp crates/stuff-worker/orange/package.json crates/stuff-worker/orange/pnpm-lock.yaml crates/stuff-worker/orange/pnpm-workspace.yaml "$context/orange/"
cp crates/stuff-worker/orange/Dockerfile "$context/Dockerfile"
remote=".cache/stuff-orange/$release"
ssh "$host" "mkdir -p $remote"
rsync -a --delete "$context/" "$host:$remote/"
ssh "$host" "docker build --platform linux/amd64 -t stuff:$release $remote >&2 && docker image inspect stuff:$release --format '{{.Os}}/{{.Architecture}}' >&2"
echo "stuff:$release"
