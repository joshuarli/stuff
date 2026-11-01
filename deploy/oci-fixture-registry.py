#!/usr/bin/env python3
"""Read-only OCI registry oracle for the checked-in fixture layout.

This is a test oracle, not the registry. It serves `tests/fixtures/oci/layout`
with the OCI Distribution 1.1 read endpoints so the conformance harness and the
pull scripts have a known-good target before `myoci serve` exists. It never
accepts writes.

Usage:
  oci-fixture-registry.py --root tests/fixtures/oci/layout [--repository example/app] [--port 0]
  oci-fixture-registry.py --root tests/fixtures/oci/layout --inspect
"""

import argparse
import hashlib
import json
import os
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlsplit

OCI_INDEX = "application/vnd.oci.image.index.v1+json"
MANIFEST_TYPES = {
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
}
DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}")
REPO_RE = re.compile(r"[a-z0-9]+(?:[._-][a-z0-9]+)*(?:/[a-z0-9]+(?:[._-][a-z0-9]+)*)*")
TAG_RE = re.compile(r"[A-Za-z0-9_][A-Za-z0-9._-]{0,127}")


class Layout:
    def __init__(self, root, repository):
        self.root = root
        self.repository = repository
        self.blobs = {}
        for name in sorted(os.listdir(os.path.join(root, "blobs", "sha256"))):
            path = os.path.join(root, "blobs", "sha256", name)
            data = open(path, "rb").read()
            digest = "sha256:" + hashlib.sha256(data).hexdigest()
            if digest != "sha256:" + name:
                raise SystemExit(f"fixture blob {name} hashes to {digest}")
            self.blobs[digest] = data
        self.top = json.loads(open(os.path.join(root, "index.json"), "rb").read())
        self.manifest_types = {}
        self.blob_types = {}
        self.referrers = {}
        self.tags = {}
        for desc in self.top["manifests"]:
            self.walk(desc)
            ref = (desc.get("annotations") or {}).get("org.opencontainers.image.ref.name")
            if ref:
                self.tags[ref] = desc
        for desc in self.manifests_with_subject():
            self.referrers.setdefault(desc["subject"]["digest"], []).append(desc["descriptor"])
        for value in self.referrers.values():
            value.sort(key=lambda item: item["digest"])

    def body(self, desc):
        data = self.blobs.get(desc["digest"])
        if data is None:
            raise SystemExit(f"fixture descriptor {desc['digest']} has no blob")
        if len(data) != desc["size"]:
            raise SystemExit(f"fixture descriptor {desc['digest']} size differs")
        return data

    def walk(self, desc):
        data = self.body(desc)
        media = desc.get("mediaType", "")
        if media in MANIFEST_TYPES:
            self.manifest_types[desc["digest"]] = media
            parsed = json.loads(data)
            if parsed.get("config"):
                self.blob_types[parsed["config"]["digest"]] = parsed["config"]["mediaType"]
            for layer in parsed.get("layers", []):
                self.blob_types[layer["digest"]] = layer["mediaType"]
            for child in parsed.get("manifests", []):
                self.walk(child)
        else:
            self.blob_types[desc["digest"]] = media

    def manifests_with_subject(self):
        found = []
        for digest, media in self.manifest_types.items():
            parsed = json.loads(self.blobs[digest])
            subject = parsed.get("subject")
            if not subject:
                continue
            found.append(
                {
                    "subject": subject,
                    "descriptor": {
                        "mediaType": media,
                        "digest": digest,
                        "size": len(self.blobs[digest]),
                        **({"artifactType": parsed["artifactType"]} if parsed.get("artifactType") else {}),
                        **({"annotations": parsed["annotations"]} if parsed.get("annotations") else {}),
                    },
                }
            )
        return found

    def inspect(self):
        manifests = sorted(self.manifest_types)
        blobs = sorted(set(self.blobs) - set(manifests))
        return {
            "tags": sorted(self.tags),
            "manifests": manifests,
            "blobs": blobs,
            "referrers": sorted(self.referrers),
        }


def error(handler, status, code, message, detail=None):
    body = {"errors": [{"code": code, "message": message}]}
    if detail is not None:
        body["errors"][0]["detail"] = detail
    encoded = json.dumps(body).encode()
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(encoded)))
    handler.end_headers()
    if handler.command != "HEAD":
        handler.wfile.write(encoded)


def paginate(items, query):
    n = query.get("n")
    last = query.get("last")
    if last:
        items = [item for item in items if item > last]
    if n:
        try:
            count = int(n[0])
        except ValueError:
            return None
        if count < 0:
            return None
        items = items[:count]
    return items


def handler_for(layout):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def send(self, status, body, content_type, headers=None):
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            for name, value in (headers or {}).items():
                self.send_header(name, value)
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(body)

        def do_HEAD(self):
            self.do_GET()

        def do_GET(self):
            parts = urlsplit(self.path)
            path = unquote(parts.path)
            query = parse_qs(parts.query)
            if path in ("/v2", "/v2/"):
                return self.send(
                    200,
                    b"{}",
                    "application/json",
                    {"Docker-Distribution-Api-Version": "registry/2.0"},
                )
            m = re.fullmatch(r"/v2/(.+)/manifests/([^/]+)", path)
            if m:
                return self.manifest(m.group(1), m.group(2))
            m = re.fullmatch(r"/v2/(.+)/blobs/(sha256:[0-9a-f]{64})", path)
            if m:
                return self.blob(m.group(1), m.group(2))
            m = re.fullmatch(r"/v2/(.+)/tags/list", path)
            if m:
                return self.tags(m.group(1), query)
            m = re.fullmatch(r"/v2/(.+)/referrers/(sha256:[0-9a-f]{64})", path)
            if m:
                return self.referrers(m.group(1), m.group(2), query)
            return error(self, 404, "NAME_UNKNOWN", "not found")

        def manifest(self, repo, reference):
            if repo != layout.repository:
                return error(self, 404, "NAME_UNKNOWN", "repository unknown")
            if DIGEST_RE.fullmatch(reference):
                digest = reference
            elif TAG_RE.fullmatch(reference):
                if reference not in layout.tags:
                    return error(self, 404, "MANIFEST_UNKNOWN", "manifest unknown")
                digest = layout.tags[reference]["digest"]
            else:
                return error(self, 400, "TAG_INVALID", "tag invalid")
            media = layout.manifest_types.get(digest)
            if media is None:
                return error(self, 404, "MANIFEST_UNKNOWN", "manifest unknown")
            data = layout.blobs[digest]
            return self.send(200, data, media, {
                "Docker-Content-Digest": digest,
                "Cache-Control": "public, max-age=31536000, immutable",
            })

        def blob(self, repo, digest):
            if repo != layout.repository:
                return error(self, 404, "NAME_UNKNOWN", "repository unknown")
            if digest in layout.manifest_types or digest not in layout.blobs:
                return error(self, 404, "BLOB_UNKNOWN", "blob unknown")
            data = layout.blobs[digest]
            media = layout.blob_types.get(digest, "application/octet-stream")
            headers = {
                "Docker-Content-Digest": digest,
                "Accept-Ranges": "bytes",
                "Cache-Control": "public, max-age=31536000, immutable",
            }
            rng = self.headers.get("Range")
            if rng:
                m = re.fullmatch(r"bytes=(\d+)-(\d*)", rng.strip())
                if not m:
                    return self.send(200, data, media, headers)
                start = int(m.group(1))
                end = int(m.group(2)) if m.group(2) else len(data) - 1
                end = min(end, len(data) - 1)
                if start >= len(data) or start > end:
                    return self.send(416, b"", media, {"Content-Range": f"bytes */{len(data)}"})
                return self.send(
                    206,
                    data[start : end + 1],
                    media,
                    {**headers, "Content-Range": f"bytes {start}-{end}/{len(data)}"},
                )
            return self.send(200, data, media, headers)

        def tags(self, repo, query):
            if repo != layout.repository:
                return error(self, 404, "NAME_UNKNOWN", "repository unknown")
            tags = sorted(layout.tags)
            page = paginate(tags, query)
            if page is None:
                return error(self, 400, "PAGINATION_NUMBER_INVALID", "invalid n")
            body = json.dumps({"name": repo, "tags": page}).encode()
            headers = {"Content-Type": "application/json"}
            if page and len(page) < len(tags):
                headers["Link"] = f'</v2/{repo}/tags/list?n={len(page)}&last={page[-1]}>; rel="next"'
            return self.send(200, body, "application/json", headers)

        def referrers(self, repo, subject, query):
            items = layout.referrers.get(subject, []) if repo == layout.repository else []
            artifact_type = query.get("artifactType", [None])[0]
            headers = {}
            if artifact_type is not None:
                items = [item for item in items if item.get("artifactType") == artifact_type]
                headers["OCI-Filters-Applied"] = "artifactType"
            digests = [item["digest"] for item in items]
            page = paginate(digests, query)
            if page is None:
                return error(self, 400, "PAGINATION_NUMBER_INVALID", "invalid n")
            selected = [item for item in items if item["digest"] in page]
            body = json.dumps(
                {"schemaVersion": 2, "mediaType": OCI_INDEX, "manifests": selected}
            ).encode()
            return self.send(200, body, OCI_INDEX, headers)

    return Handler


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=os.path.join(os.path.dirname(__file__), "..", "tests", "fixtures", "oci", "layout"))
    parser.add_argument("--repository", default="example/app")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--inspect", action="store_true")
    args = parser.parse_args()
    layout = Layout(args.root, args.repository)
    if args.inspect:
        print(json.dumps(layout.inspect(), indent=2, sort_keys=True))
        return 0
    server = ThreadingHTTPServer(("127.0.0.1", args.port), handler_for(layout))
    print(f"PORT={server.server_address[1]}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
