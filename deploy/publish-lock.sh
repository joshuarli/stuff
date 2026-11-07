#!/bin/sh
# Serialize writers of one registry's bucket on this machine: stuff, the
# mypypi container, and gc (deploy/r2_common.py publish_lock takes the same
# flock). The caller traps release_publish_lock.
set -eu

bucket_lock_path() {
  printf '%s/%s/publish.lock\n' "${STUFF_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/stuff}" "${1#stuff-}"
}

publish_lock_try() {
  if command -v flock >/dev/null 2>&1; then
    flock -n 9
    return
  fi
  if ! command -v perl >/dev/null 2>&1; then
    echo "publish: neither flock nor perl is available for the publish lock" >&2
    return 2
  fi
  perl -MFcntl=:flock -e 'open(my $fh, ">&=9") or exit 2; exit(flock($fh, LOCK_EX | LOCK_NB) ? 0 : 1)'
}

legacy_lock_is_stale() {
  [ -n "$(find "$PUBLISH_LOCK" -prune -mmin +60 2>/dev/null)" ]
}

clear_legacy_lock_dir() {
  while [ -d "$PUBLISH_LOCK" ]; do
    pid=$(cat "$PUBLISH_LOCK/pid" 2>/dev/null || true)
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && ! legacy_lock_is_stale; then
      echo "publish: waiting for active writer ($PUBLISH_LOCK)"
      sleep 2
      continue
    fi
    rm -f "$PUBLISH_LOCK/pid" 2>/dev/null || true
    if ! rmdir "$PUBLISH_LOCK" 2>/dev/null; then
      [ -d "$PUBLISH_LOCK" ] || continue
      echo "publish: cannot clear legacy lock directory $PUBLISH_LOCK" >&2
      return 1
    fi
  done
}

acquire_publish_lock() {
  PUBLISH_LOCK=$1
  mkdir -p "$(dirname "$PUBLISH_LOCK")"
  clear_legacy_lock_dir
  exec 9>>"$PUBLISH_LOCK"
  while :; do
    if publish_lock_try; then
      break
    else
      status=$?
    fi
    [ "$status" -eq 1 ] || return "$status"
    echo "publish: waiting for active writer ($PUBLISH_LOCK)"
    sleep 2
  done
}

release_publish_lock() {
  if [ -n "${PUBLISH_LOCK:-}" ]; then
    exec 9>&-
    PUBLISH_LOCK=""
  fi
}
