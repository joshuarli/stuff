# mypypi

Python wheel index: resolves and builds wheels, validates them on the host,
and publishes immutable generations under a `current` pointer.

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

## Publish per platform

Wheels are validated on a native runner, so one host publishes only its own
platform. Packages that exist only for other operating systems (for example
`pyobjc-*` on Linux) need `platforms = [...]` in their catalog entry, and
wheels that fail validation by design need `exclude_platforms` or
`validate_skip_imports` entries with a reason.

One generation serves every platform: run build, validate, and publish once
per platform against the same catalog, state directory, and site. Each
`publish` adds that platform's wheels to the current generation and keeps the
rest. Each container run holds the host's mypypi publish lock, so platforms
publish one at a time. Linux platforms run in a container of the matching
architecture, which is a native runner for mypypi's check (the binary is
compiled inside the container, so `current_platform()` reports the
container's architecture):

```sh
deploy/mypypi-container.sh arm64 linux-aarch64 build
deploy/mypypi-container.sh arm64 linux-aarch64 validate
deploy/mypypi-container.sh arm64 linux-aarch64 publish
deploy/mypypi-container.sh amd64 linux-x86_64 validate   # Rosetta/QEMU on an arm64 host
```

`linux-x86_64` on an arm64 host runs under emulation; the check accepts it
because the userspace and binary are x86_64, not because the code runs on x86
silicon. Publish the Linux wheels from a real x86_64 runner when that matters.
`add` takes no `--platform` and rewrites the shared catalog; run it before the
per-platform commands. The container mounts the same
`$STUFF_STATE_HOME/mypypi/{packages.toml,pyproject.toml,state,site}` paths as
`stuff publish pypi`, so every platform must use that one state home.

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
variables. There is no filesystem or network sandbox: an sdist build backend
runs as the publisher's user and can read any file that user can.

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
