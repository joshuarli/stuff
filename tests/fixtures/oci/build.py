#!/usr/bin/env python3
"""Build the deterministic OCI image layout used by the myoci tests.

The layout holds a two-platform image index tagged `stable`, an untagged
referrer manifest whose subject is the linux/arm64 image manifest, and every
config and layer blob those manifests reference. Output is deterministic: fixed
timestamps, sorted JSON, and gzip with mtime 0.

Usage: build.py [--out DIR] [--check]
"""

import argparse
import gzip
import hashlib
import io
import json
import os
import shutil
import sys
import tarfile
import tempfile

CREATED = "2026-11-01T00:00:00Z"
OCI_INDEX = "application/vnd.oci.image.index.v1+json"
OCI_MANIFEST = "application/vnd.oci.image.manifest.v1+json"
OCI_CONFIG = "application/vnd.oci.image.config.v1+json"
OCI_LAYER_GZIP = "application/vnd.oci.image.layer.v1.tar+gzip"
OCI_EMPTY = "application/vnd.oci.empty.v1+json"
SBOM_TYPE = "application/vnd.stuff.sbom.v1+json"


def dumps(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def sha256(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def platform_layer(name):
    tar = io.BytesIO()
    with tarfile.open(fileobj=tar, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        content = f"hello from {name}\n".encode()
        info = tarfile.TarInfo("hello.txt")
        info.size = len(content)
        info.mtime = 0
        info.uid = info.gid = 0
        info.uname = info.gname = ""
        info.mode = 0o644
        archive.addfile(info, io.BytesIO(content))
    raw = tar.getvalue()
    return gzip.compress(raw, compresslevel=9, mtime=0), sha256(raw)


def image_config(architecture, diff_id):
    return dumps(
        {
            "architecture": architecture,
            "config": {"Cmd": ["/hello"]},
            "created": CREATED,
            "history": [{"created": CREATED, "created_by": "stuff oci fixture"}],
            "os": "linux",
            "rootfs": {"diff_ids": [diff_id], "type": "layers"},
        }
    )


def build():
    blobs = {}

    def put(data):
        digest = sha256(data)
        blobs[digest] = data
        return {"digest": digest, "size": len(data)}

    empty_config = put(b"{}")
    sbom = put(dumps({"spdxVersion": "SPDX-2.3", "name": "stuff-oci-fixture"}))
    platforms = []
    for architecture in ("arm64", "amd64"):
        layer, diff_id = platform_layer(architecture)
        layer_desc = put(layer)
        config_desc = put(image_config(architecture, diff_id))
        manifest = dumps(
            {
                "schemaVersion": 2,
                "mediaType": OCI_MANIFEST,
                "config": {
                    "mediaType": OCI_CONFIG,
                    "digest": config_desc["digest"],
                    "size": config_desc["size"],
                },
                "layers": [
                    {
                        "mediaType": OCI_LAYER_GZIP,
                        "digest": layer_desc["digest"],
                        "size": layer_desc["size"],
                    }
                ],
            }
        )
        manifest_desc = put(manifest)
        platforms.append((architecture, manifest_desc))

    index = dumps(
        {
            "schemaVersion": 2,
            "mediaType": OCI_INDEX,
            "manifests": [
                {
                    "mediaType": OCI_MANIFEST,
                    "digest": desc["digest"],
                    "size": desc["size"],
                    "platform": {"architecture": architecture, "os": "linux"},
                }
                for architecture, desc in platforms
            ],
        }
    )
    index_desc = put(index)

    arm64 = dict(platforms)["arm64"]
    referrer = dumps(
        {
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST,
            "artifactType": SBOM_TYPE,
            "annotations": {"org.opencontainers.image.created": CREATED},
            "config": {
                "mediaType": OCI_EMPTY,
                "digest": empty_config["digest"],
                "size": empty_config["size"],
            },
            "layers": [
                {
                    "mediaType": SBOM_TYPE,
                    "digest": sbom["digest"],
                    "size": sbom["size"],
                }
            ],
            "subject": {
                "mediaType": OCI_MANIFEST,
                "digest": arm64["digest"],
                "size": arm64["size"],
            },
        }
    )
    referrer_desc = put(referrer)

    top = dumps(
        {
            "schemaVersion": 2,
            "mediaType": OCI_INDEX,
            "manifests": [
                {
                    "mediaType": OCI_INDEX,
                    "digest": index_desc["digest"],
                    "size": index_desc["size"],
                    "annotations": {"org.opencontainers.image.ref.name": "stable"},
                },
                {
                    "mediaType": OCI_MANIFEST,
                    "digest": referrer_desc["digest"],
                    "size": referrer_desc["size"],
                },
            ],
        }
    )
    return blobs, top, index_desc, platforms, referrer_desc


def write(root, blobs, top):
    layout = os.path.join(root, "layout")
    os.makedirs(os.path.join(layout, "blobs", "sha256"), exist_ok=True)
    for digest, data in blobs.items():
        with open(os.path.join(layout, "blobs", "sha256", digest.split(":", 1)[1]), "wb") as f:
            f.write(data)
    with open(os.path.join(layout, "index.json"), "wb") as f:
        f.write(top + b"\n")
    with open(os.path.join(layout, "oci-layout"), "w", encoding="utf-8") as f:
        f.write('{"imageLayoutVersion":"1.0.0"}\n')


def files(root):
    found = {}
    for base, _, names in os.walk(root):
        for name in sorted(names):
            path = os.path.join(base, name)
            found[os.path.relpath(path, root)] = open(path, "rb").read()
    return found


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default=os.path.dirname(os.path.abspath(__file__)))
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    blobs, top, index_desc, platforms, referrer_desc = build()
    if args.check:
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, blobs, top)
            expected = files(os.path.join(os.path.dirname(os.path.abspath(__file__)), "layout"))
            actual = files(os.path.join(tmp, "layout"))
            if expected != actual:
                only_expected = sorted(set(expected) - set(actual))
                only_actual = sorted(set(actual) - set(expected))
                changed = sorted(k for k in set(expected) & set(actual) if expected[k] != actual[k])
                print(f"layout differs: missing={only_expected} extra={only_actual} changed={changed}", file=sys.stderr)
                return 1
        print("ok: oci fixture matches")
        return 0
    shutil.rmtree(os.path.join(args.out, "layout"), ignore_errors=True)
    write(args.out, blobs, top)
    print(f"index {index_desc['digest']}")
    for architecture, desc in platforms:
        print(f"{architecture} {desc['digest']}")
    print(f"referrer {referrer_desc['digest']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
