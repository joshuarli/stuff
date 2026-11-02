.PHONY: help deploy republish-mycrates republish-mynpm republish-mypypi republish-oci search-bundle test-search build test clippy smoke smoke-deployed pipecheck worker-build worker-bundle worker-check worker-deploy worker-test sync gc serve test-sync test-publish-lock test-r2 test-r2-live test-r2-gc-blobs test-oci-conformance-fixture test-oci-conformance test-oci-conformance-push test-oci-pull test-oci-push test-oci-publish oci-bench test-r2-multipart fleet-ingest test-mypypi-native test-mypypi-linux-native

help: ## Show this help
	@grep -E '^[a-z-]+: ## ' $(MAKEFILE_LIST) | sort | awk -F': ## ' '{printf "  make %-14s %s\n", $$1, $$2}'

build: ## Build the full workspace
	cargo build --workspace

test: ## Run the full workspace test suite
	cargo test --workspace

clippy: ## Lint the workspace (denies warnings)
	cargo clippy --locked --all-targets -- -D warnings

smoke: ## Drive the registry binaries with real npm/uv toolchains (needs Node 26, pinned npm, uv + CPython 3.15, repair tooling, network)
	deploy/ecosystem-smoke.sh

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

search-bundle: ## Rebuild the inline search script from web/ (needs npm ci in crates/stuff-worker)
	cd $(WORKER_DIR) && node_modules/.bin/vite build --config vite.search.config.ts

test-search: ## Run the package search tests
	node --test $(SEARCH_DIR)/web/search.test.ts

worker-build: ## Compile the worker wasm glue into crates/stuff-worker/build
	cd $(WORKER_DIR) && npm run build:wasm

worker-check: ## Validate the worker build and deploy plan without credentials
	cd $(WORKER_DIR) && npm run build && cf deploy --prebuilt --dry-run

worker-bundle: ## Build the Cloudflare worker bundle once for test and deploy
	cd $(WORKER_DIR) && npm run build

worker-test: worker-bundle ## Exercise the worker bundle under the workerd runtime
	cd $(WORKER_DIR) && npm run test:runtime

worker-deploy: worker-bundle ## Deploy the worker bundle tested by worker-test
	cd $(WORKER_DIR) && cf deploy --prebuilt --mode production

DEPLOY_JOBS ?= 3
OCI_DEPLOY ?=

deploy: ## Test and deploy the worker, republish registries, then check the live site
	@cf auth whoami >/dev/null
	$(MAKE) worker-test worker-deploy
	CF_AUTH_CHECKED=1 $(MAKE) -j$(DEPLOY_JOBS) republish-mycrates republish-mynpm republish-mypypi $(if $(OCI_DEPLOY),republish-oci)
	$(MAKE) smoke-deployed

republish-mycrates: ## Republish the mycrates site from state
	deploy/publish mycrates --republish

republish-mynpm: ## Republish the mynpm site from state
	deploy/publish mynpm --republish

republish-mypypi: ## Republish the mypypi site from state
	deploy/publish mypypi --republish

republish-oci: ## Republish the OCI site from state
	deploy/publish oci --republish

SITE_DIR ?= .work/site
BUCKET ?=
LOCAL_STATE ?= .work/r2-local

sync: ## Sync a published site dir to its R2 bucket (SITE_DIR= BUCKET=, LOCAL=1 for the local simulation)
	@test -n "$(BUCKET)" || (echo "usage: make sync SITE_DIR=<dir> BUCKET=<name> [LOCAL=1]"; exit 1)
	@if [ -n "$(LOCAL)" ]; then CF_LOCAL_STATE="$(LOCAL_STATE)" deploy/r2-sync.sh $(SITE_DIR) $(BUCKET); \
	else deploy/with-publish-lock.sh $(BUCKET) deploy/r2-sync.sh $(SITE_DIR) $(BUCKET); fi

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

test-sync: ## Check r2-sync pagination and idempotency on the local R2 simulation ([SYNC_OBJECTS=1100])
	@mkdir -p .work/tmp
	TMPDIR="$${TMPDIR:-.work/tmp}" deploy/test-r2-sync.sh $(or $(SYNC_OBJECTS),1100)

test-publish-lock: ## Check the cross-repo publish lock serializes writers ([WAITERS=4])
	@mkdir -p .work/tmp
	TMPDIR="$${TMPDIR:-.work/tmp}" deploy/test-publish-lock.sh $(or $(WAITERS),4)

test-r2: ## Check S3 uploads, credentials, multipart SigV4, pagination, and promotion without network access
	@mkdir -p .work/tmp
	TMPDIR="$${TMPDIR:-.work/tmp}" deploy/test-r2-sync.sh $(or $(SYNC_OBJECTS),1100)
	deploy/test-r2-pull.sh
	python3 deploy/test-r2-pull.py
	deploy/test-prune-generations.sh
	python3 deploy/test-r2-pull-listing.py
	python3 deploy/test-r2-gc.py
	python3 deploy/test-r2-multipart.py
	python3 deploy/test-r2-plan.py
	python3 deploy/test-r2-promote.py
	python3 deploy/test-r2-put.py
	python3 deploy/test-r2-temp-credentials.py
	python3 deploy/test-r2-gc-blobs.py

test-oci-publish: ## Check OCI publication, blob sync, blob GC, rollback, and adoption offline
	deploy/test-oci-publish.sh

test-r2-gc-blobs: ## Check OCI blob GC against a fake S3 endpoint
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
