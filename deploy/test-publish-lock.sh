#!/bin/sh
# Check that deploy/publish-lock.sh admits one writer at a time, including
# while reclaiming a stale directory lock left by an older release, that
# gc's Python lock, with-publish-lock.sh, and mypypi-container.sh contend on
# the same file, and that the npm and pypi helpers exit on INT, TERM, and HUP
# instead of continuing without the lock.
# Usage: deploy/test-publish-lock.sh [WAITERS]
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
waiters=${1:-4}
base=${TMPDIR:-.work/tmp}
mkdir -p "$base"
tmp=$(mktemp -d "$base/publish-lock.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
lock=$tmp/publish.lock
log=$tmp/writers.log

mkdir "$lock"
printf '%s\n' 999999 >"$lock/pid"

pids=""
i=0
while [ "$i" -lt "$waiters" ]; do
  i=$((i + 1))
  (
    . "$here/publish-lock.sh"
    acquire_publish_lock "$lock"
    printf 'enter %s\n' "$i" >>"$log"
    sleep 0.2
    printf 'exit %s\n' "$i" >>"$log"
    release_publish_lock
  ) &
  pids="$pids $!"
done
for pid in $pids; do
  wait "$pid"
done

enters=$(awk '$1 == "enter" { n++ } END { print n + 0 }' "$log")
[ "$enters" -eq "$waiters" ] || {
  echo "expected $waiters writers, saw $enters"
  exit 1
}
awk '
  $1 == "enter" {
    if (held != "") {
      printf "overlap: %s entered while %s held the lock\n", $2, held
      bad = 1
    }
    held = $2
  }
  $1 == "exit" {
    if (held != $2) {
      printf "unmatched exit: %s (holder %s)\n", $2, held
      bad = 1
    }
    held = ""
  }
  END {
    if (held != "") {
      printf "missing exit for %s\n", held
      bad = 1
    }
    exit bad
  }
' "$log"

echo "publish lock serialized $waiters writers"

export STUFF_STATE_HOME="$tmp/home"
order=$tmp/order.log
"$here/with-publish-lock.sh" stuff-demo sh -c "echo shell-enter >>'$order'; sleep 0.5; echo shell-exit >>'$order'" &
shell_pid=$!
sleep 0.2
python3 -I -c '
import sys
sys.path.insert(0, sys.argv[1])
from r2_common import publish_lock
with publish_lock("stuff-demo"):
    open(sys.argv[2], "a").write("python-enter\n")
' "$here" "$order" >/dev/null
wait "$shell_pid"
[ "$(cat "$order")" = "$(printf 'shell-enter\nshell-exit\npython-enter')" ] || {
  echo "gc lock did not wait for the shell holder:"
  cat "$order"
  exit 1
}
echo "gc and shell writers share the registry lock"

mkdir -p "$tmp/bin"
cat > "$tmp/bin/docker" <<FAKE
#!/bin/sh
case \$1 in
  run) echo container-run >>'$order' ;;
  wait) echo 0 ;;
esac
FAKE
chmod +x "$tmp/bin/docker"
: >"$order"
"$here/with-publish-lock.sh" stuff-mypypi sh -c "echo host-enter >>'$order'; sleep 0.5; echo host-exit >>'$order'" &
host_pid=$!
sleep 0.2
PATH="$tmp/bin:$PATH" "$here/mypypi-container.sh" arm64 linux-aarch64 publish >/dev/null
wait "$host_pid"
[ "$(cat "$order")" = "$(printf 'host-enter\nhost-exit\ncontainer-run')" ] || {
  echo "container run did not wait for the host publish lock:"
  cat "$order"
  exit 1
}
echo "container runs share the host mypypi lock"

fake=$tmp/fake
mkdir -p "$fake/deploy" "$fake/target/debug" "$tmp/fakebin"
cp "$here/publish-lock.sh" "$fake/deploy/publish-lock.sh"
printf '#!/bin/sh\necho "r2-sync $*" >>"$LOG"\n' >"$fake/deploy/r2-sync.sh"
printf '#!/bin/sh\necho "prune $*" >>"$LOG"\n' >"$fake/deploy/prune-generations.sh"
printf '#!/bin/sh\nexit 0\n' >"$fake/Cargo.toml"
for tool in cargo node uv python; do
  printf '#!/bin/sh\nexit 0\n' >"$tmp/fakebin/$tool"
  chmod +x "$tmp/fakebin/$tool"
done
printf '#!/bin/sh\necho 26.0.0\n' >"$tmp/fakebin/node"
printf '#!/bin/sh\necho "mynpm $*" >>"$LOG"\nexec 9>&-\ncase " $* " in\n  *" fetch "*) echo $$ >"$CHILD"; : >"$READY"; sleep 60 ;;\nesac\nexit 0\n' >"$fake/target/debug/mynpm"
printf '#!/bin/sh\necho "mypypi $*" >>"$LOG"\nexec 9>&-\ncase " $* " in\n  *" add "*) echo $$ >"$CHILD"; : >"$READY"; sleep 60 ;;\nesac\nexit 0\n' >"$fake/target/debug/mypypi"
chmod +x "$fake/deploy/r2-sync.sh" "$fake/deploy/prune-generations.sh" \
  "$fake/target/debug/mynpm" "$fake/target/debug/mypypi"

export LOG="$tmp/helper.log" CHILD="$tmp/helper-child.pid" READY="$tmp/helper-ready"

check_signal_exit() {
  helper=$1
  next=$2
  : >"$LOG"
  : >"$tmp/helper.err"
  rm -f "$CHILD" "$READY"
  if [ "$helper" = mynpm ]; then
    env STUFF_DIR="$fake" PATH="$tmp/fakebin:$PATH" LEGACY_STATE_DIR="$tmp/none" \
      LOCKFILE="$tmp/lock.yaml" PROJECT=demo STUFF_STATE_HOME="$tmp/home" \
      STATE_DIR="$tmp/signal-$helper/state" SITE_DIR="$tmp/signal-$helper/site" \
      CONFIG_FILE="$tmp/signal-$helper/config.json" \
      "$here/publish-$helper.sh" 2>>"$tmp/helper.err" &
  else
    env STUFF_DIR="$fake" PATH="$tmp/fakebin:$PATH" LEGACY_STATE_DIR="$tmp/none" \
      PACKAGES="demo==1.0" PYTHON="$tmp/fakebin/python" PLATFORM=test \
      STUFF_STATE_HOME="$tmp/home" STATE_DIR="$tmp/signal-$helper/state" \
      SITE_DIR="$tmp/signal-$helper/site" CATALOG_FILE="$tmp/signal-$helper/packages.toml" \
      CONFIG_FILE="$tmp/signal-$helper/pyproject.toml" \
      "$here/publish-$helper.sh" 2>>"$tmp/helper.err" &
  fi
  pid=$!
  i=0
  while [ ! -f "$READY" ]; do
    i=$((i + 1))
    if [ "$i" -gt 200 ]; then
      echo "$helper did not reach $next"
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
      exit 1
    fi
    sleep 0.05
  done
  child=$(cat "$CHILD")
  kill -TERM "$child" "$pid" 2>/dev/null || true
  status=0
  wait "$pid" 2>/dev/null || status=$?
  [ "$status" -eq 143 ] || {
    echo "$helper exited $status after TERM, want 143"
    cat "$LOG" "$tmp/helper.err"
    exit 1
  }
  if grep -q " $next " "$LOG"; then
    echo "$helper continued to $next after TERM:"
    cat "$LOG" "$tmp/helper.err"
    exit 1
  fi
  python3 -c 'import fcntl, sys; fcntl.flock(open(sys.argv[1], "a"), fcntl.LOCK_EX | fcntl.LOCK_NB)' \
    "$tmp/home/$helper/publish.lock"
  echo "$helper releases its lock and exits 143 on TERM"
}

check_signal_exit mynpm publish
check_signal_exit mypypi build
