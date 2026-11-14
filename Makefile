.PHONY: help release lint ts-lint repomod-test repomod-lint deps bench bench-cli deploy stuff-release install republish-mycrates republish-mynpm republish-mypypi republish-mirror republish-oci search-bundle catalog-css test-search build test test-mycrates test-stuff test-stuff-parity test-stuff-e2e clippy smoke smoke-deployed pipecheck worker-build worker-bundle worker-check worker-deploy worker-test gc serve stuff test-publish-lock test-r2 test-r2-live test-r2-gc-blobs test-oci-conformance-fixture test-oci-conformance test-oci-conformance-push test-oci-pull test-oci-push test-oci-publish oci-bench test-r2-multipart fleet-ingest test-mypypi-native test-mypypi-linux-native

help: ## Show this help
	@grep -E '^[a-z-]+: ## ' $(MAKEFILE_LIST) | sort | awk -F': ## ' '{printf "  make %-14s %s\n", $$1, $$2}'

build: ## Build the full workspace
	cargo build --workspace

test: test-mycrates repomod-test ## Run the full workspace test suite
	cargo test --workspace

lint: clippy repomod-lint ts-lint ## Lint the Rust workspace, repomod, and the TypeScript projects

ts-lint: ## Lint and type-check the Worker TypeScript
	pnpm --dir crates/stuff-worker install --frozen-lockfile
	pnpm --dir crates/stuff-worker run lint

repomod-test: ## Run the repomod tests
	@uv run --group dev --quiet pytest

repomod-lint: ## Lint and type-check repomod
	@uv run --group dev --quiet ruff check repomod tests/repomod bench
	@uv run --group dev --quiet ty check

PORT ?= 8765
RUNS ?= 3

deps: ## Sync, snapshot, and serve the fleet dependency dashboard ([PORT=8765] [PRIVATE=1])
	@uv run repomod sync
	@uv run repomod deps snapshot
	@git add -A -- fleet/dependencies/v1
	@if ! git diff --cached --quiet -- fleet/dependencies/v1; then \
		git commit -m "chore(deps): snapshot fleet dependency inventory" \
			-m "The dashboard renders this tracked inventory; refresh it so the pages reflect the synced checkouts."; \
	fi
	@uv run repomod deps render --status $(if $(filter 1,$(PRIVATE)),--include-private)
	@exec srv -p $(PORT) .work/dependencies/dashboard

bench: ## Measure dependency dashboard render phases, cold and warm ([RUNS=3])
	@uv run python bench/deps_perf.py phases --mode both --runs $(RUNS)

bench-cli: ## Measure the real deps render command, cold and warm ([RUNS=3])
	@uv run python bench/deps_perf.py workflow --mode both --runs $(RUNS)

test-mycrates: ## Run the mycrates Python tests (needs cargo for the publish proof)
	cd crates/mycrates && uv run --group dev --project ../.. pytest tests

test-stuff: ## Run the stuff crate tests
	cargo test -p stuff

test-stuff-parity: ## Run the stuff crate parity suite against the shared fake S3
	cargo test -p stuff

clippy: ## Lint the workspace (denies warnings)
	cargo clippy --locked --all-targets -- -D warnings

smoke: ## Drive the registry binaries with real npm/uv toolchains (needs Node 26, pinned npm, uv + CPython 3.15, repair tooling, network)
	deploy/ecosystem-smoke.sh

test-stuff-e2e: ## End-to-end publish, pull, serve, and real npm/uv install against fake S3
	deploy/test-stuff-e2e.sh

smoke-deployed: ## Read-only live check of the deployed registries (needs built stuff binary, network)
	cargo build -p stuff
	deploy/deployed-smoke.sh

test-mypypi-native: ## Build, repair, validate, publish, and install a native C/C++ sdist (needs repair tooling)
	deploy/test-mypypi-native.sh

test-mypypi-linux-native: ## Run the native wheel conformance inside the pinned manylinux 2.28 image
	deploy/test-linux.sh sh -c 'MYPYPI_EXPECT_MANYLINUX=2.28 exec deploy/test-mypypi-native.sh'

pipecheck: ## Prove one registry's publication never touches another's output
	deploy/check-pipeline-independence.sh

WORKER_DIR ?= crates/stuff-worker
SEARCH_DIR ?= crates/stuff-catalog

search-bundle: ## Rebuild the inline search script from web/ (needs pnpm install --frozen-lockfile in crates/stuff-worker)
	cd $(WORKER_DIR) && node_modules/.bin/vite build --config vite.search.config.ts

UI_DASHBOARD_DIST := $(WORKER_DIR)/node_modules/@laputa-systems/ui-dashboard/dist
CATALOG_CSS := $(SEARCH_DIR)/assets/ui-dashboard

catalog-css: ## Copy the pinned ui-dashboard stylesheets into stuff-catalog; fail if the committed copies are stale
	cd $(WORKER_DIR) && pnpm install --frozen-lockfile
	mkdir -p $(CATALOG_CSS)
	cp $(UI_DASHBOARD_DIST)/tokens.css $(UI_DASHBOARD_DIST)/styles.css $(UI_DASHBOARD_DIST)/styles/layout.css $(UI_DASHBOARD_DIST)/styles/data.css $(CATALOG_CSS)/
	@git diff --exit-code -- $(CATALOG_CSS)

test-search: ## Run the package search tests
	node --test $(SEARCH_DIR)/web/search.test.ts

worker-build: ## Compile the worker wasm glue into crates/stuff-worker/build
	cd $(WORKER_DIR) && pnpm run build:wasm

worker-check: ## Validate the worker build and deploy plan without credentials
	cd $(WORKER_DIR) && pnpm run build && cf deploy --prebuilt --dry-run

worker-bundle: ## Build the Cloudflare worker bundle once for test and deploy
	cd $(WORKER_DIR) && pnpm run build

worker-test: worker-bundle ## Exercise the worker bundle under the workerd runtime
	cd $(WORKER_DIR) && pnpm run test:runtime

worker-deploy: worker-bundle ## Deploy the worker bundle tested by worker-test
	cd $(WORKER_DIR) && cf deploy --prebuilt --mode production

DEPLOY_JOBS ?= 3
OCI_DEPLOY ?=
STUFF ?= target/release/stuff

HOST = $(shell rustc -vV | sed -n 's/^host: //p')
TARGET ?= $(subst -unknown-linux-gnu,-unknown-linux-musl,$(HOST))

release: ## Build the stuff CLI for TARGET with the dist profile; run Linux musl builds inside the docker/musl image
	MUSL_TARGET="$(TARGET)" MUSL_BUILD_MODE=release-static $(if $(findstring -linux-musl,$(TARGET)),musl-cargo,cargo) \
	  build --locked --profile dist -p stuff -Z build-std=std -Z build-std-features= --target $(TARGET)

stuff-release: ## Build the release stuff CLI once
	cargo build -q --release -p stuff

PREFIX ?= $(HOME)/.local

install: stuff-release ## Install the release stuff CLI to $(PREFIX)/bin ([PREFIX=~/.local])
	install -d "$(PREFIX)/bin"
	install -m 755 target/release/stuff "$(PREFIX)/bin/stuff"
	"$(PREFIX)/bin/stuff" lock-path --registry cargo >/dev/null

deploy: ## Test and deploy the worker, republish registries, then check the live site
	@cf auth whoami >/dev/null
	$(MAKE) worker-test worker-deploy
	CF_AUTH_CHECKED=1 $(MAKE) -j$(DEPLOY_JOBS) republish-mycrates republish-mynpm republish-mypypi republish-mirror $(if $(OCI_DEPLOY),republish-oci)
	$(MAKE) smoke-deployed

republish-mycrates: stuff-release ## Republish the mycrates site from state
	$(STUFF) republish cargo

republish-mynpm: stuff-release ## Republish the mynpm site from state
	$(STUFF) republish npm

republish-mypypi: stuff-release ## Republish the mypypi site from state
	$(STUFF) republish pypi

republish-oci: stuff-release ## Republish the OCI site from state
	$(STUFF) republish oci

republish-mirror: stuff-release ## Republish the static artifact mirror from its manifest
	$(STUFF) republish mirror

SITE_DIR ?= .work/site

GC_BUCKETS ?= stuff-mycrates stuff-mynpm stuff-mypypi
GC_KEEP_PREVIOUS ?= 1
GC_KEEP_NEWER ?= all
GC_DRY_RUN ?=
GC_JOBS ?=

gc: ## Reclaim R2 storage from old generations and unreferenced shared objects, reporting before/after (GC_BUCKETS= GC_KEEP_PREVIOUS= GC_KEEP_NEWER= GC_DRY_RUN=1 GC_JOBS=)
	@test -n "$(GC_BUCKETS)" || (echo "usage: make gc GC_BUCKETS=<bucket...>"; exit 1)
	python3 deploy/r2-gc.py $(GC_BUCKETS) --keep-previous $(GC_KEEP_PREVIOUS) --keep-newer $(GC_KEEP_NEWER) $(if $(filter-out 0,$(GC_DRY_RUN)),--dry-run,) $(if $(GC_JOBS),--jobs $(GC_JOBS),)

SERVE_PORT ?= 8000

serve: ## Serve a published site dir locally (SITE_DIR= [SERVE_PORT=])
	cargo run -q -p stuff-server -- --bind 127.0.0.1:$(SERVE_PORT) --route /=$(SITE_DIR)

STUFF_SITE := .work/site-stuff
STUFF_PORT ?= 8080

stuff: ## Render the unified registry page from the live catalogs and serve it locally
	@mkdir -p $(STUFF_SITE)
	cargo run -q -p stuff -- render --output $(STUFF_SITE)/index.html
	cargo run -q -p stuff -- render --output $(STUFF_SITE)/llms.txt
	@exec srv -p $(STUFF_PORT) $(STUFF_SITE)

test-publish-lock: ## Check the cross-repo publish lock serializes writers ([WAITERS=4])
	@mkdir -p .work/tmp
	TMPDIR="$${TMPDIR:-.work/tmp}" deploy/test-publish-lock.sh $(or $(WAITERS),4)

test-r2: ## Check S3 uploads, multipart SigV4, pull, gc, and pagination without network access
	@mkdir -p .work/tmp
	cargo build -q -p stuff
	TMPDIR="$${TMPDIR:-.work/tmp}" deploy/test-r2-pull.sh
	python3 deploy/test-r2-pull.py
	deploy/test-prune-generations.sh
	python3 deploy/test-r2-pull-listing.py
	python3 deploy/test-r2-gc.py
	python3 deploy/test-r2-multipart.py
	python3 deploy/test-r2-put.py
	python3 deploy/test-r2-gc-blobs.py

test-oci-publish: ## Check stuff OCI publication, blob GC, rollback, and adoption against a fake S3 endpoint
	deploy/test-oci-publish.sh

test-store-copy: ## Check the resumable bucket copy and verification against two fake S3 endpoints
	@mkdir -p .work/tmp
	TMPDIR="$${TMPDIR:-$(CURDIR)/.work/tmp}" sh deploy/test-store-copy.sh

test-r2-gc-blobs: ## Check OCI blob GC against a fake S3 endpoint
	cargo build -q -p stuff
	python3 deploy/test-r2-gc-blobs.py

test-r2-live: ## Opt-in live R2 multipart acceptance (R2_ACCEPTANCE_BUCKET=... and R2_S3_*)
	python3 deploy/test-r2-multipart-live.py

test-oci-conformance-fixture: ## Prove the pinned OCI conformance harness against the checked-in fixture
	deploy/oci-conformance.sh --fixture-layout tests/fixtures/oci/layout

OCI_CONFORMANCE_WORK ?= .work/oci-conformance
test-oci-conformance: ## Run the pinned read conformance suite against myoci serve
	rm -rf $(OCI_CONFORMANCE_WORK)
	cargo run -q -p myoci -- init --state-dir $(OCI_CONFORMANCE_WORK)/state
	cargo run -q -p myoci -- import --state-dir $(OCI_CONFORMANCE_WORK)/state --layout tests/fixtures/oci/layout --repository example/app
	cargo run -q -p myoci -- publish --state-dir $(OCI_CONFORMANCE_WORK)/state --site-dir $(OCI_CONFORMANCE_WORK)/site
	deploy/oci-conformance.sh --myoci-state $(OCI_CONFORMANCE_WORK)/state --myoci-site $(OCI_CONFORMANCE_WORK)/site

test-oci-conformance-push: ## Run the pinned push conformance suite against myoci serve and the writer
	deploy/test-oci-conformance-push.sh

oci-bench: ## Benchmark the local OCI fixture and rewrite docs/oci-benchmarks.md ([RUNS=5])
	deploy/oci-bench.sh --runs $(or $(RUNS),5)

test-oci-pull: ## Serve the fixture locally and pull it with curl, Docker, and ORAS
	deploy/test-oci-pull.sh

test-oci-push: ## Push a two-platform image through the front and writer
	deploy/test-oci-push.sh

test-r2-multipart: ## Send a 1 GiB blob through the writer's R2 sink (live S3 credentials)
	deploy/test-r2-multipart.sh

FLEET_OUT ?= .work/fleet

fleet-ingest: ## Ingest fleet lists into local per-registry sites, no R2 (FLEET_OUT= CARGO_LIST= NPM_LIST= PYPI_LIST=)
	deploy/fleet-ingest.sh $(FLEET_OUT) $(if $(CARGO_LIST),--cargo $(CARGO_LIST)) $(if $(NPM_LIST),--npm $(NPM_LIST)) $(if $(PYPI_LIST),--pypi $(PYPI_LIST))

.PHONY: clean
clean:
	rm -rf .venv
	find . -path './.git' -prune -o -path './.work' -prune -o -name __pycache__ -type d -prune -exec rm -rf {} +
	find . -path './.git' -prune -o -path './.work' -prune -o -name node_modules -type d -prune -exec rm -rf {} +
	find . -path './.git' -prune -o -path './.work' -prune -o -name target -type d -prune -exec rm -rf {} +
