#!/bin/sh
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

root=$(CDPATH= cd -- "$here/.." && pwd)
(cd "$root" && cargo build -q -p stuff)
STUFF=${CARGO_TARGET_DIR:-"$root/target"}/debug/stuff
export STUFF
export STUFF_STATE_HOME="$tmp/home" STUFF_CONFIG_HOME="$tmp/config"
mkdir -p "$STUFF_CONFIG_HOME"
cat >"$STUFF_CONFIG_HOME/config.json" <<'JSON'
{"cargo": {"bucket": "c-cargo"}, "npm": {"bucket": "c-npm"}, "pypi": {"bucket": "c-pypi"},
 "oci": {"bucket": "c-oci", "blobs_bucket": "c-oci-blobs"}}
JSON
order=$tmp/order.log

cat >"$tmp/s3.py" <<'PY'
import http.server, sys, time
delay = float(sys.argv[1])
class Missing(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        time.sleep(delay)
        body = b"<Error><Code>NoSuchKey</Code></Error>"
        self.send_response(404)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    do_HEAD = do_GET
    def log_message(self, *args):
        pass
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Missing)
print(server.server_port, flush=True)
server.serve_forever()
PY
python3 -I "$tmp/s3.py" 1 >"$tmp/s3-slow.port" 2>/dev/null &
slow_pid=$!
python3 -I "$tmp/s3.py" 0 >"$tmp/s3-fast.port" 2>/dev/null &
fast_pid=$!
trap 'kill "$slow_pid" "$fast_pid" 2>/dev/null || true; rm -rf "$tmp"' EXIT
for _ in $(seq 50); do
  [ -s "$tmp/s3-slow.port" ] && [ -s "$tmp/s3-fast.port" ] && break
  sleep 0.1
done
slow=http://127.0.0.1:$(cat "$tmp/s3-slow.port")
fast=http://127.0.0.1:$(cat "$tmp/s3-fast.port")

native_rollback() {
  R2_S3_ENDPOINT=$1 R2_S3_ACCESS_KEY_ID=test R2_S3_SECRET_ACCESS_KEY=secret \
    "$STUFF" rollback "$2" 0123456789abcdef01234567 >/dev/null 2>>"$tmp/native.err" || true
  echo native-exit >>"$order"
}
helper_lock() {
  python3 -I -c '
import sys, time
sys.path.insert(0, sys.argv[1])
from r2_common import publish_lock
with publish_lock(sys.argv[2]):
    open(sys.argv[3], "a").write("helper-enter\n")
    time.sleep(float(sys.argv[4]))
    open(sys.argv[3], "a").write("helper-exit\n")
' "$here" "$1" "$order" "$2" >"$tmp/helper.out"
}
expect_waited() {
  grep -q "waiting for active writer" "$1" || {
    echo "$2"
    exit 1
  }
}
expect_order() {
  [ "$(cat "$order")" = "$(printf "$1")" ] || {
    echo "$2:"; cat "$tmp/native.err"
    cat "$order"
    exit 1
  }
}

for pair in cargo:c-cargo npm:c-npm pypi:c-pypi oci:c-oci oci:c-oci-blobs; do
  registry=${pair%%:*}
  bucket=${pair#*:}
  : >"$order"
  native_rollback "$slow" "$registry" &
  native=$!
  sleep 0.3
  helper_lock "$bucket" 0
  wait "$native"
  expect_waited "$tmp/helper.out" "helper for $bucket did not wait for stuff rollback $registry"

  : >"$order"
  helper_lock "$bucket" 1 &
  helper=$!
  sleep 0.3
  native_rollback "$fast" "$registry"
  wait "$helper"
  expect_order 'helper-enter\nhelper-exit\nnative-exit' "stuff rollback $registry did not wait for the helper on $bucket"
done
echo "native publishers and Python helpers share each registry lock"

: >"$order"
helper_lock c-npm 1 &
helper=$!
sleep 0.3
(
  . "$here/publish-lock.sh"
  acquire_bucket_lock c-npm
  echo shell-enter >>"$order"
) >"$tmp/shell.out"
wait "$helper"
expect_waited "$tmp/shell.out" "shell bucket lock did not wait for the Python holder"
expect_order 'helper-enter\nhelper-exit\nshell-enter' "shell bucket lock did not wait for the Python holder"
if (. "$here/publish-lock.sh"; acquire_bucket_lock unknown-bucket) 2>/dev/null; then
  echo "an unconfigured bucket acquired a lock"
  exit 1
fi
echo "shell and Python helpers resolve the same lock and reject unknown buckets"

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
native_rollback "$slow" pypi &
native=$!
sleep 0.3
PATH="$tmp/bin:$PATH" "$here/mypypi-container.sh" arm64 linux-aarch64 publish >"$tmp/container.out"
wait "$native"
expect_waited "$tmp/container.out" "container run did not wait for stuff's pypi lock"
expect_order 'native-exit\ncontainer-run' "container ran before stuff's pypi lock was released"
echo "container runs share the pypi registry lock"
