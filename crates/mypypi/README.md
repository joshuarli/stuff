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

`stuff publish pypi` selects the glibc Linux targets `linux-x86_64` and
`linux-aarch64`, the musl Linux targets `linux-musl-x86_64` and
`linux-musl-aarch64`, and the macOS targets `macos-x86_64` and `macos-arm64`.
A glibc target accepts `manylinux_2_5` through `manylinux_2_28` tags (and their
legacy aliases); a musl target accepts `musllinux_1_0` through `musllinux_1_2`
and never a manylinux tag, so an Alpine host can install from the mirror only
when each package has a musllinux wheel, or a source archive built with
`--allow-sdist`. A package with neither fails the publish for the musl targets.
`mypypi add` resolves dependencies for the musl targets with
`uv pip compile --python-platform x86_64-unknown-linux-musl` (or
`aarch64-unknown-linux-musl`).

The local target is `linux-musl-<arch>` when the interpreter named by `PYTHON`
(default `python3`) is an ELF executable whose program interpreter is
`ld-musl-*`, and `linux-<arch>` for any other Linux interpreter. Native sdist
builds on a musl target are repaired with `auditwheel` to `musllinux_1_2`
whatever `linux_repair_policy` says; that setting applies to the glibc targets.

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
variables. The allowlist is defense in depth; the execution boundary below is
what keeps package code away from the publisher's files.

Build dependencies are pinned by `deploy/build-constraints.txt`, a
hash-pinned `uv` universal constraints file that `mypypi` embeds in the
binary and applies with `--build-constraints`. An sdist whose
`build-system.requires` cannot be satisfied from that set fails instead of
selecting anything else; when `--require-build-hashes` is on (the default, in
`mypypi build` and in `stuff publish pypi`), a distribution whose bytes do not
match the pinned hashes fails instead of installing.

The pins are a policy, not a universal resolver: an sdist whose backend caps
its requirement below or above a pin (for example `flit_core>=3.9,<4` against
`flit-core==4.2.0`) fails loudly rather than selecting an unvetted version.
Keep the pins compatible with the ranges the ecosystem actually declares.

Refresh the file with:

```sh
$EDITOR deploy/build-constraints.in
TMPDIR="$PWD/.work/tmp" uv pip compile deploy/build-constraints.in \
  --universal --generate-hashes -o deploy/build-constraints.txt
```

`deploy/build-constraints.in` lists build backends and build tools by name
(with version ranges where the ecosystem standard caps them, such as
`flit-core<4`); the compile resolves the transitive closure pinned to exact
versions with sha256 hashes for every supported platform and interpreter.
Commit both files together, and keep `cargo test -p mypypi` green:
`embedded_build_constraints_pin_hashes_for_every_requirement` fails when a
pin loses its hashes. To admit a failed sdist, add the missing backend to the
`.in` file and refresh rather than loosening anything.

## Execution boundary

Package code never runs as the bare publisher. Two steps execute it, and both
run inside an operating-system sandbox that `mypypi` builds around the child:

- the sdist build backend, through `uv build --offline` (reached only with
  `--allow-sdist`), after a separate network stage resolves the build
  requirements, and
- the import check of every new native-target wheel, `python -I -c
  __import__(...)`, which runs even without `--allow-sdist`.

Installing a wheel (`uv venv`, `uv pip install --no-index --only-binary`),
repairing it (`auditwheel`, `delocate`), and the catalog's `custom_prebuild`
commands run outside the sandbox. None of them executes package code, and
`custom_prebuild` is curated catalog configuration.

A confined child can read the operating system's own files, the interpreter
and its prefix, the `uv` executable, the Cargo `bin` and rustup `toolchains`
directories named by `CARGO_HOME`/`RUSTUP_HOME` (default `~/.cargo`,
`~/.rustup`), the selected Xcode bundle on macOS, the `--build-constraints`
file in effect (by default the embedded `deploy/build-constraints.txt`), and
the certificate bundles named by `SSL_CERT_FILE` and its siblings. It can
write only to its own scratch: the build staging directory, or the validation
scratch directory. Every other path, including the publisher's home, the state
directory, and other staging directories, is unreadable and unwritable.

The sdist build backend itself never reaches the network. When an sdist
declares `build-system.requires`, `mypypi` first runs a confined prime stage
with the same boundary plus network access: it resolves those requirements
against the default index with `uv pip compile --constraints` pinned by the
effective constraints file and downloads them with hashes verified into a
stage-local uv cache (`UV_CACHE_DIR` under the staging directory). The wheel
then builds inside the boundary with `--offline`, so the resolution can only
draw from that cache, and `--require-build-hashes` fails the build when a
resolved distribution does not match the constraints file. Import checks have
no network. The prime stage touches only the declared requirements;
`custom_prebuild` commands still install whatever they fetch, outside the
sandbox, as curated catalog configuration.

| Host | Mechanism | Posture |
| --- | --- | --- |
| macOS | `/usr/bin/sandbox-exec` with a generated profile | `(deny default)` plus `system.sb`, then explicit allows. Credential directories under the home (`.ssh`, `.aws`, `.config`, `.gnupg`, `.cargo/credentials*`, `Library/Keychains`, and similar) are also denied last. Metadata (`stat`) stays readable because `uv` treats `EPERM` on an ancestor's `pyproject.toml` or `.venv` as fatal; contents and directory listings do not. The Keychain and process listing are unreachable from inside. |
| Linux | `bwrap` (bubblewrap) | New user, PID, IPC, UTS and cgroup namespaces, `--cap-drop ALL`, `--die-with-parent`, and `--unshare-net` for builds and imports (the prime stage keeps the network). Only `/usr`, `/bin`, `/sbin`, `/lib*`, `/etc`, `/opt`, a tmpfs `/tmp`, and a tmpfs over the publisher's home are mounted, then the granted paths. An unmounted path does not exist, so a probe sees `FileNotFoundError` where macOS reports `PermissionError`. |

The boundary fails closed. When `sandbox-exec` or `bwrap` is missing, or the
host cannot create the sandbox, the stage fails with `execution boundary
unavailable (...)` (exit 1, `Dependency`) and no package code runs. There is no
opt-out. A grant that would contain the publisher's home is refused as a
configuration error (exit 2). Linux needs unprivileged user namespaces. In a
container, allow them with `--security-opt seccomp=unconfined --security-opt
apparmor=unconfined --security-opt systempaths=unconfined` (Docker's default
AppArmor profile denies the mounts `bwrap` makes); `deploy/test-linux.sh` and
`deploy/mypypi-container.sh` pass all three, and `deploy/mypypi-linux.Dockerfile`
installs `bubblewrap`. On Ubuntu 24.04 and later, `kernel.apparmor_restrict_unprivileged_userns=0`
is also required; CI sets it.

Credentials stay in the publisher process. `stuff` mints R2 credentials in
its own process and uploads from it; children receive only the allowlisted
variables above, never `R2_S3_*`, `AWS_*`, or any `cf` state, and cannot
inspect the publisher's memory.

The boundary does not make the built wheel trustworthy. A confined backend can
still emit a malicious wheel, so `mypypi` treats it as untrusted: identity,
metadata, RECORD hashes, tags, and architectures are checked, and a native
wheel is imported inside the boundary before publication. A build backend can
still read anything in its scratch, which is why the backend builds with no
network; the prime stage is the only part of an sdist build with one.
Hash-pinned build constraints bound which dependencies a build selects, not
what they do; `--require-build-hashes` makes an unpinned or tampered
distribution fail instead of install. `sandbox-exec` is deprecated by Apple
but is what the system tools use; its negative tests run with `cargo test -p mypypi`.

Tests: `cargo test -p mypypi` runs a hostile wheel (its import tries to read a
sentinel outside scratch and to write outside scratch) and a hostile sdist
backend (same attempts), each beside an unconfined control that proves the
attempt would succeed, plus a benign wheel import and a benign sdist build.
The wheel and sdist tests need `uv` and a CPython on `PATH` and skip with a
message when either is missing. On Linux the `sandbox` unit tests skip when
`bwrap` cannot start, while every test that would run package code fails
closed. Three ignored
tests need more: `an_sdist_fetches_its_build_requirements_inside_the_boundary`
reaches the network and exercises the prime stage and the offline backend
end to end, `rust_sdist_build_finds_the_host_toolchain` needs Rust
(`deploy/ecosystem-smoke.sh`), and `native_sdist_builds_a_repaired_extension_wheel`
(`deploy/test-mypypi-native.sh`) builds a C and C++ extension, repairs it, and
imports it inside the boundary.

## Configuration

Project settings live in the configured `pyproject.toml`. In addition to the
targets under `[tool.mypypi.targets]`, `[tool.mypypi] public_base_url` names
the origin clients should use:

```toml
[tool.mypypi]
public_base_url = "https://pypi.example.net"

[tool.mypypi.targets]
platforms = ["linux-x86_64", "linux-musl-x86_64", "macos-arm64"]
linux_repair_policy = "manylinux_2_28"
```

Platform names are `linux-x86_64`, `linux-aarch64`, `linux-musl-x86_64`,
`linux-musl-aarch64`, `macos-x86_64`, and `macos-arm64`.

The publisher writes that origin into the generation's `llms.txt`, so a
deployment at another origin publishes pip instructions for its own index.
When the setting is absent it defaults to `https://pypi.17166969.xyz`.

## Publication format

Generations are identified by `mypypi-simple-html-v5`. Older
`mypypi-simple-html-v4` generations stay readable and are not rewritten.
First-party rows add `first_party` to the manifest row without a version change.
