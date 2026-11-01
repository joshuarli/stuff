# OCI image layout fixture

A deterministic two-platform OCI image layout for `myoci` import, publish,
pull, and conformance tests. It is not a published image.

- `build.py` regenerates `layout/`; `build.py --check` fails if the checked-in
  bytes differ.
- `layout/index.json` tags the image index `stable` and also lists an untagged
  referrer manifest whose `subject` is the linux/arm64 image manifest.
- `layout/blobs/sha256/` holds the index, both platform manifests, their config
  and gzip layer blobs, the empty config, and the SBOM layer.

Pinned digests:

```
index    sha256:04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388
arm64    sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4
amd64    sha256:d4ae9ea60e8f1f455de9959f23d711014d5f97e9d85ce82a6ecc769a61d1c65c
referrer sha256:3a91f0fb91009745e3a72651ec98d02a4c758c4f580bd9ab8683467f1f264f25
subject  sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4
```

Exercise it with the read-only oracle and the pinned conformance suite:

```sh
python3 deploy/oci-fixture-registry.py --root tests/fixtures/oci/layout --port 5000
python3 deploy/oci-fixture-registry.py --root tests/fixtures/oci/layout --inspect
make test-oci-conformance-fixture
docker pull 127.0.0.1:5000/example/app:stable
```
