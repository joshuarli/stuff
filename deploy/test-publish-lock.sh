#!/bin/sh
# Check that deploy/publish-lock.sh admits one writer at a time, including
# while reclaiming a stale directory lock left by an older release, and that
# gc's Python lock, with-publish-lock.sh, and mypypi-container.sh contend on
# the same file.
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
