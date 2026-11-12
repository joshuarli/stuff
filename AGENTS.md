# AGENTS.md

stuff holds the infrastructure for every repository its owner maintains:

- **Registries** (`crates/`, `deploy/`): four static registries share one Rust serving platform
  and the `stuff` publisher. The Python 3.11 standard-library `mycrates` package remains an
  archive/maintenance tool and publication parity oracle.
- **Fleet** (`repomod/`, `fleet/`, `docker/musl/`): repomod, a Python 3.15 tool, manages the
  repositories in `fleet/catalog.toml`. It syncs checkouts, checks policy and opens pull requests
  for fixes, releases binaries, and keeps the dependency inventory in `fleet/dependencies/` with its
  dashboard. `fleet/policy/` holds its policy data and `docker/musl/` the fleet's musl toolchain
  image.

## Commands

Run everything from the repository root.

- Build/test/lint: `make build`, `make test` (Rust workspace, mycrates, and repomod), `make lint` (clippy, ruff, ty).
- Rust toolchain: `rust-toolchain.toml`; Worker package/tool pins: `crates/stuff-worker/package.json` and `Cargo.lock`.
- Python oracle: `make test-mycrates` (needs Cargo for the native publication proof).
- repomod: `uv sync`, then `uv run repomod --help`; `make repomod-test`, `make repomod-lint`. Set `TMPDIR` to `.work/tmp` for commands that create scratch data.
- Fleet dashboard: `make deps` syncs the checkouts, re-snapshots and commits `fleet/dependencies/v1/`, renders, and serves it; `make dashboard-ts` rebuilds its scripts after changing `repomod/dependencies/dashboard_ts/`; `make bench` measures the render.
- Search: `make test-search`; after changing `crates/stuff-catalog/web/`, run `make search-bundle` and commit `assets/search.js` (install Worker dependencies first).
- Worker: `make worker-test`, `make worker-check`; build instructions are in its README below.
- Offline maintenance: `make test-r2`, `make test-publish-lock`; `make test-r2-live` accesses real R2 and is opt-in.
- Ecosystem integration: `deploy/ecosystem-smoke.sh` needs the pinned package managers, CPython, native repair tools, and network. Use `TMPDIR="$PWD/.work/tmp"` for campaign commands.
- OCI checks: `make test-oci-conformance`, `make test-oci-conformance-push`, `make test-oci-pull`, `make test-oci-push`, `make test-oci-publish`, `make oci-bench`.

## Topic map

| Topic | Owner |
| --- | --- |
| Publishing, credentials, state, deployment, recovery | [docs/PUBLISHING.md](docs/PUBLISHING.md); native implementation in `crates/stuff/src/publish/` |
| Cargo archive and Python oracle | [crates/mycrates/AGENTS.md](crates/mycrates/AGENTS.md) |
| Cargo formats and shared catalog schema | [docs/mycrates-contracts.md](docs/mycrates-contracts.md) |
| Legacy Cargo state migration | [docs/mycrates-migration.md](docs/mycrates-migration.md) |
| npm inventory and publication | [crates/mynpm/README.md](crates/mynpm/README.md) |
| Wheel acquisition and validation | [crates/mypypi/README.md](crates/mypypi/README.md) |
| OCI protocol and storage | [docs/oci-design.md](docs/oci-design.md); [protocol API](crates/stuff-oci/README.md) |
| OCI CLI, server, writer, benchmarks | [crates/myoci/README.md](crates/myoci/README.md) |
| OCI benchmark results and the direct blob origin (A11) | [docs/oci-benchmarks.md](docs/oci-benchmarks.md), [docs/oci-a11.md](docs/oci-a11.md) |
| Worker host, WASM build, runtime tests | [crates/stuff-worker/README.md](crates/stuff-worker/README.md) |
| Shared storage and generation activation | `crates/stuff-store/src/`; `Site` is used by myoci, while mynpm/mypypi activate their own sites |
| Shared HTTP policy and native adapter | `crates/stuff-http/src/`, `crates/stuff-server/src/` |
| Catalog rendering and search | `crates/stuff-catalog/src/`, `crates/stuff-catalog/web/` |
| URL and Cargo shard parsing | `crates/stuff-url/src/lib.rs`, `crates/stuff-cargo-shard/src/lib.rs` |
| Maintenance helpers and offline fakes | `deploy/`; commands in `Makefile` |
| Using repomod: catalog, sync, lint, fix, release | [docs/fleet.md](docs/fleet.md) |
| repomod architecture, sync, repair, and state ownership | [docs/fleet-design.md](docs/fleet-design.md) |
| Catalog tags and visibility | [docs/tagging.md](docs/tagging.md) |
| Rules, fixers, and musl publication | [docs/policy-development.md](docs/policy-development.md) |
| Dependency policy checks and the registry policy the fleet follows | [docs/audit.md](docs/audit.md); `fleet/registries.toml` |
| Tracked dependency model and extraction | [docs/dependency-inventory.md](docs/dependency-inventory.md) |
| Dependency dashboard, owners, mirror coverage, browser checks, benchmarks | [docs/dependency-dashboard.md](docs/dependency-dashboard.md) |
| Repository layout used by repomod | `repomod/paths.py` |
| Checkout removal contract | [docs/rfc/RPMD-74-checkout-removal.md](docs/rfc/RPMD-74-checkout-removal.md) |
| Remaining musl rollout decisions | [docs/rfc/RPMD-85-musl-rollout.md](docs/rfc/RPMD-85-musl-rollout.md) |
| Campaign records | [docs/campaigns/](docs/campaigns/) |
| Deferred fixes | [TODO.md](TODO.md) |
| Draft designs | [STUF-45](docs/rfc/STUF-45-python-execution.md), [STUF-62](docs/rfc/STUF-62-shared-index-bytes.md) |

## Invariants

- `stuff publish cargo|npm|pypi|oci` is the publication entry point; the Python Cargo writer stays only as a parity oracle.
- One registry's publication never rebuilds another registry's output.
- Keep package semantics per ecosystem; share serving/storage/generation code without a universal `Package` abstraction.
- First-party Rust forbids unsafe code; Python mycrates uses only the standard library.
- Keep npm lifecycle scripts disabled during acquisition and validation; retain native-target execution checks for wheels.
- Preserve published generation readability; format contracts belong in the owning registry docs.
- Keep fixtures while parity consumers use them, including the executable examples in `crates/mycrates/README.md`.
- Keep `crates/h12tiny-*` sources byte-identical to imported upstream `dcdf4f2`; change upstream before updating the import.
- The registries must stay buildable when their own package registries are unavailable: Cargo, npm,
  and PyPI dependencies of the registry crates resolve from upstream, not from stuff. The catalog
  exempts stuff from repomod's `registry-sources` rule for this reason.
- repomod manages stuff like any other catalogued repository. It reads the canonical checkout under
  `~/repos`, so develop in a separate clone; `fix` publishes only from an unchanged canonical
  checkout.
- repomod reads the catalogs the registries publish (`<origin>/current/catalog.json`); change the
  catalog schema and its repomod reader together.
- Stuff files use Apache-2.0; imported crates retain their license fields.

## Fleet constraints

- Run `uv run repomod sync` before fleet work, so canonical checkouts reflect their remotes.
- `fleet/catalog.toml` is curated user data. Read it before catalog work and preserve its comments and repository selection.
- Do not run `fix` against the live catalog while changing repomod. Tests use local Git repositories and fake GitHub responses; use copied catalogs for mutation tests.
- Canonical checkouts are user data. Never reset, stash, clean, switch, or discard changes in them.
- Sync may fetch and only fast-forward clean checkouts on their default branch. Fixers use detached worktrees and publish only from an unchanged canonical checkout.
- Remote-write commands are `topics apply`, `push`, `fix`, `merge`, and `release`; `fix` pushes branches and opens pull requests. `pins refresh` writes local approved pins.
- Keep Git subprocesses, GitHub operations, synchronization, and policy checks in their owning modules under `repomod/`. Resolve repository files through `repomod/paths.py`.
- Keep rules read-only and fixers as explicit, validated file plans. Do not add databases, background workers, or generic workflow frameworks.
- Keep pull-request publication in the fix flow. Do not add rollout tracking, patch storage, or per-fixer clone directories.
