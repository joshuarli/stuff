#!/bin/sh
# Serialize one registry's whole publication transaction across source repos.
# Sourced by deploy/publish-*.sh. The caller traps release_publish_lock.
set -eu

acquire_publish_lock() {
  PUBLISH_LOCK=$1
  mkdir -p "$(dirname "$PUBLISH_LOCK")"
  while ! mkdir "$PUBLISH_LOCK" 2>/dev/null; do
    if [ -f "$PUBLISH_LOCK/pid" ]; then
      pid=$(cat "$PUBLISH_LOCK/pid" 2>/dev/null || true)
      if [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; then
        rm -rf "$PUBLISH_LOCK"
        continue
      fi
    else
      sleep 1
      if [ ! -f "$PUBLISH_LOCK/pid" ]; then
        rm -rf "$PUBLISH_LOCK"
        continue
      fi
    fi
    echo "publish: waiting for active writer ($PUBLISH_LOCK)"
    sleep 2
  done
  printf '%s\n' "$$" > "$PUBLISH_LOCK/pid"
}

release_publish_lock() {
  if [ -n "${PUBLISH_LOCK:-}" ]; then
    rm -rf "$PUBLISH_LOCK"
    PUBLISH_LOCK=""
  fi
}
