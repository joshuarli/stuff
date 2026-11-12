# mypypi

Python wheel index: resolves and builds wheels, validates native wheels by
installation/import and foreign wheels by static inspection, and publishes
immutable generations under a `current` pointer.

## Ingest pins from lockfiles

`mypypi add` takes exact `name==version` pins and resolves their
dependencies with `uv pip compile` for every configured target. Pins can come
from the registry packages of `uv.lock` files and from requirements files
(comments and `--hash` continuations are ignored):

```sh
mypypi --catalog packages.toml --config pyproject.toml --state-dir state add \
  --uv-lock ../app/uv.lock --requirements pins.txt extra==1.0
```

Only `[[package]]` entries whose `source` is a registry are read; virtual,
editable, path, and git packages are skipped. Run `add` once per lockfile:
one invocation rejects two versions of the same package, and lockfiles in a
fleet usually disagree. The lockfile hashes are not carried into the catalog;
compare them with the published wheel hashes after publishing.

## Publish and validate platforms

`stuff publish pypi` selects Linux x86_64/aarch64 and macOS x86_64/arm64.
Publication installs and imports new wheels for the local target. For foreign
targets it inspects metadata, RECORD hashes, tags, and native binary headers
without running the package. A foreign target marked validated therefore does
not imply an import check on that operating system.

Standalone `mypypi validate` uses the same distinction. Select the local target
with `--platform` and use a matching runner or the Linux container for
installation and import checks:

```sh
deploy/mypypi-container.sh arm64 linux-aarch64 build
deploy/mypypi-container.sh arm64 linux-aarch64 validate
deploy/mypypi-container.sh arm64 linux-aarch64 publish
deploy/mypypi-container.sh amd64 linux-x86_64 validate   # Rosetta/QEMU on an arm64 host
```

An emulated x86_64 userspace satisfies the native-target check on an arm64
host. Use a real x86_64 runner when execution on that hardware matters.
`add` rewrites the catalog and takes no `--platform`; run it before these
commands. Catalog entries can restrict `platforms` or declare
`exclude_platforms`/`validate_skip_imports` with a reason.

The container helper uses legacy `$STUFF_STATE_HOME/mypypi/` paths. The Rust
publisher uses `$STUFF_STATE_HOME/pypi/`; their lock identities currently
differ. Run them separately until the maintenance-lock finding in
[TODO.md](../../TODO.md) is resolved.

## First-party wheels

`stuff publish pypi --wheel PATH` hosts a pure-Python wheel that exists nowhere
upstream (see [docs/PUBLISHING.md](../../docs/PUBLISHING.md)). The wheel passes
the same static inspection as a foreign-target wheel on every configured target
and must carry only `none-any` tags; it is not installed or imported. Its
`manifest.json` row carries the extra key `"first_party": true`, which is absent
from every other row, so existing manifests and generation ids are unchanged.
`schema_version` stays 1. The live manifest is the only place the record
persists, so mirror publishes and `stuff republish pypi` carry it forward.
A first-party name cannot reuse a mirrored name, a mirror publish cannot add
another version of a first-party name, and a first-party version is immutable.
Older binaries reject a manifest that contains a first-party row.

## Build environment

`add`, `build`, and `validate` require `uv` 0.13.6 (pinned in
`deploy/uv-version`), the version installed in the Linux build container;
another version on `PATH` fails the command before it reads or writes the
catalog or state, instead of resolving dependencies differently per platform.

Native wheels are repaired with `auditwheel` on Linux and `delocate` on
macOS. Their versions are pinned in `deploy/mypypi-repair-versions`, and
`deploy/provision-mypypi-repair.sh` installs the matching tool into the build
interpreter. The Linux container installs it from that same pin.

```sh
PYTHON=$(uv python find 3.15) deploy/provision-mypypi-repair.sh
```

`deploy/mypypi-linux.Dockerfile` builds on the digest-pinned
`manylinux_2_28` images named in `deploy/manylinux_2_28-amd64` and
`deploy/manylinux_2_28-arm64`, so Linux compilations link against glibc 2.28
instead of the host's newer glibc. The image also pins its CPython patch in
`deploy/mypypi-linux-python-version`. `deploy/test-mypypi-native.sh` builds a C
and C++ sdist through build, repair, validation, publication, and
installation, then checks the wheel's platform tag and its glibc or macOS
deployment floor. Linux hosts run it through `make test-mypypi-linux-native`
inside the pinned image; a newer host glibc cannot produce the 2.28 floor.

Build subprocesses get an allowlisted environment and run with a scratch
`HOME`, `CARGO_HOME`, and `RUSTUP_HOME`. The scratch homes link to the
installed Cargo bin and rustup toolchains, so Rust-backed sdists find the host
toolchain without inheriting the publisher's Cargo cache or credential
variables. Sdist backends and native wheel imports both run as the publisher
without filesystem or network isolation. Requiring prebuilt wheels avoids
build backends but still executes package code during native validation.
The execution-boundary decision is [STUF-45](../../docs/rfc/STUF-45-python-execution.md).

## Configuration

Project settings live in the configured `pyproject.toml`. In addition to the
targets under `[tool.mypypi.targets]`, `[tool.mypypi] public_base_url` names
the origin clients should use:

```toml
[tool.mypypi]
public_base_url = "https://pypi.example.net"

[tool.mypypi.targets]
platforms = ["linux-x86_64", "macos-arm64"]
linux_repair_policy = "manylinux_2_28"
```

The publisher writes that origin into the generation's `llms.txt`, so a
deployment at another origin publishes pip instructions for its own index.
When the setting is absent it defaults to `https://pypi.17166969.xyz`.

## Publication format

Generations are identified by `mypypi-simple-html-v5`. Older
`mypypi-simple-html-v4` generations stay readable and are not rewritten.
First-party rows add `first_party` to the manifest row without a version change.
