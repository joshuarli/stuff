#!/bin/sh
set -eu
here=$(CDPATH='' cd -- "$(dirname "$0")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/store-copy.XXXXXX")
pids=
cleanup() { for pid in $pids; do kill "$pid" 2>/dev/null || true; done; rm -rf "$work"; }
trap cleanup EXIT INT TERM
start() {
	mkfifo "$work/$1.in"
	python3 "$here/fake_s3.py" < "$work/$1.in" > "$work/$1.out" 2>"$work/$1.err" &
	pids="$pids $!"
}
port() {
	until grep -q '^PORT ' "$work/$1.out" 2>/dev/null; do sleep 0.1; done
	sed -n 's/^PORT //p' "$work/$1.out"
}
start source
exec 8>"$work/source.in"
start dest
exec 9>"$work/dest.in"
src=$(port source)
dst=$(port dest)
export STUFF_COPY_SOURCE_S3_ENDPOINT="http://127.0.0.1:$src" STUFF_COPY_SOURCE_S3_ACCESS_KEY_ID=source STUFF_COPY_SOURCE_S3_SECRET_ACCESS_KEY=source-secret STUFF_COPY_SOURCE_S3_REGION=us-east-1
export STUFF_COPY_DEST_S3_ENDPOINT="http://127.0.0.1:$dst" STUFF_COPY_DEST_S3_ACCESS_KEY_ID=dest STUFF_COPY_DEST_S3_SECRET_ACCESS_KEY=dest-secret STUFF_COPY_DEST_S3_REGION=us-east-1
export R2_LIST_PAGE=100 PYTHONPATH="$here"
python3 - "$src" <<'PY'
import importlib.util, os, sys
sys.path.insert(0, os.environ["PYTHONPATH"])
spec = importlib.util.spec_from_file_location("s3", os.path.join(os.environ["PYTHONPATH"], "r2-multipart.py"))
s3 = importlib.util.module_from_spec(spec); spec.loader.exec_module(s3)
import xml.etree.ElementTree as ET
os.environ.update(R2_S3_ACCESS_KEY_ID="source", R2_S3_SECRET_ACCESS_KEY="source-secret", R2_S3_REGION="us-east-1")
base = f"http://127.0.0.1:{sys.argv[1]}/bucket"
for index in range(250):
    status, _, _ = s3.request("PUT", f"{base}/objects/{index:04d}.txt", f"object {index}\n".encode() * (index + 1), "text/plain")
    assert status == 200, status
status, body, _ = s3.request("POST", f"{base}/blobs/layer?uploads", b"", "application/octet-stream")
upload = s3.find_text(ET.fromstring(body), "UploadId")
parts = []
for number, size in enumerate([1 << 20, 1 << 20, 12345], start=1):
    status, _, headers = s3.request("PUT", f"{base}/blobs/layer?partNumber={number}&uploadId={upload}", bytes([number]) * size, "application/octet-stream")
    assert status == 200, status
    parts.append(f"<Part><PartNumber>{number}</PartNumber><ETag>{headers.get('ETag')}</ETag></Part>")
status, _, _ = s3.request("POST", f"{base}/blobs/layer?uploadId={upload}", f"<CompleteMultipartUpload>{''.join(parts)}</CompleteMultipartUpload>".encode(), "application/octet-stream")
assert status == 200, status
PY
copy() { python3 "$here/store-copy.py" copy bucket bucket --ledger "$work/ledger.jsonl"; }
verify() { python3 "$here/store-copy.py" verify bucket bucket --ledger "$work/ledger.jsonl" "$@"; }
field() { python3 -I -c 'import json,sys; print(json.loads(sys.stdin.read())[sys.argv[1]])' "$1"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

first=$(copy 2>"$work/copy.err") || { cat "$work/copy.err" >&2; fail "first copy failed"; }
[ "$(echo "$first" | field copied)" = 251 ] || fail "first copy did not copy every object: $first"
out=$(verify --deep) || fail "verify after copy"
[ "$(echo "$out" | field etagPreserved)" = 251 ] || fail "multipart ETag was not preserved: $out"
[ "$(copy | field copied)" = 0 ] || fail "rerun copied verified objects"
printf '{"key":"interrupted' >> "$work/ledger.jsonl"
[ "$(copy | field copied)" = 0 ] || fail "recovery of a torn ledger copied verified objects"
[ "$(jaq -s length "$work/ledger.jsonl")" = 251 ] || fail "torn ledger tail was not removed"
head -n 100 "$work/ledger.jsonl" > "$work/partial" && mv "$work/partial" "$work/ledger.jsonl"
[ "$(copy | field copied)" = 151 ] || fail "resume did not copy exactly the unverified objects"
python3 - "$src" <<'PY'
import importlib.util, os, sys
sys.path.insert(0, os.environ["PYTHONPATH"])
spec = importlib.util.spec_from_file_location("s3", os.path.join(os.environ["PYTHONPATH"], "r2-multipart.py"))
s3 = importlib.util.module_from_spec(spec); spec.loader.exec_module(s3)
os.environ.update(R2_S3_ACCESS_KEY_ID="source", R2_S3_SECRET_ACCESS_KEY="source-secret", R2_S3_REGION="us-east-1")
assert s3.request("PUT", f"http://127.0.0.1:{sys.argv[1]}/bucket/objects/0007.txt", b"changed after copy\n", "text/plain")[0] == 200
PY
verify >/dev/null 2>&1 && fail "verify missed a changed source"
[ "$(copy | field recopiedChanged)" = 1 ] || fail "changed source was not re-copied"
verify --deep >/dev/null || fail "verify after re-copy"
python3 - "$dst" <<'PY'
import importlib.util, os, sys
sys.path.insert(0, os.environ["PYTHONPATH"])
spec = importlib.util.spec_from_file_location("s3", os.path.join(os.environ["PYTHONPATH"], "r2-multipart.py"))
s3 = importlib.util.module_from_spec(spec); spec.loader.exec_module(s3)
os.environ.update(R2_S3_ACCESS_KEY_ID="dest", R2_S3_SECRET_ACCESS_KEY="dest-secret", R2_S3_REGION="us-east-1")
base = f"http://127.0.0.1:{sys.argv[1]}/bucket"
assert s3.request("PUT", f"{base}/objects/0010.txt", b"corrupted\n", "text/plain")[0] == 200
assert s3.request("DELETE", f"{base}/objects/0011.txt")[0] in (200, 204)
PY
out=$(verify 2>&1) && fail "verify missed corruption"
echo "$out" | grep -q 'destination differs from the ledger: objects/0010.txt' || fail "corruption not named: $out"
echo "$out" | grep -q 'missing at destination: objects/0011.txt' || fail "missing object not named: $out"
echo "OK: copy, ETag-preserving multipart, resume, changed-source, corruption and missing-object detection, paginated listings"
