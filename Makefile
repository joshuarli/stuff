.PHONY: help deploy build test clippy smoke smoke-deployed pipecheck worker-build worker-check worker-deploy worker-test sync serve test-sync test-publish-lock test-r2 test-r2-live fleet-ingest

help: ## Show this help
	@grep -E '^[a-z-]+: ## ' $(MAKEFILE_LIST) | sort | awk -F': ## ' '{printf "  make %-14s %s\n", $$1, $$2}'

build: ## Build the full workspace
	cargo build --workspace

test: ## Run the full workspace test suite
	cargo test --workspace

clippy: ## Lint the workspace (denies warnings)
	cargo clippy --locked --all-targets -- -D warnings

smoke: ## Drive the registry binaries with real npm/uv toolchains (needs Node 26, pinned npm, uv + CPython 3.15, network)
	deploy/ecosystem-smoke.sh

smoke-deployed: ## Read-only live check of the deployed registries (needs built stuff binary, network)
	cargo build -p stuff
	deploy/deployed-smoke.sh

pipecheck: ## Prove one registry's publication never touches another's output
	deploy/check-pipeline-independence.sh

WORKER_DIR ?= crates/stuff-worker

worker-build: ## Compile the worker wasm glue into crates/stuff-worker/build
	cd $(WORKER_DIR) && npm run build:wasm

worker-check: ## Validate the worker build and deploy plan without credentials
	cd $(WORKER_DIR) && cf deploy --dry-run

worker-test: ## Build the worker bundle and exercise it under the workerd runtime
	cd $(WORKER_DIR) && npm run build:worker && npm run test:runtime

worker-deploy: ## Deploy the worker, provision R2 buckets, attach custom domains
	cd $(WORKER_DIR) && cf deploy

deploy: ## Test and deploy the worker, republish all three registries, then check the live site
	cf auth whoami
	$(MAKE) worker-test
	$(MAKE) worker-deploy
	deploy/publish mycrates --republish
	deploy/publish mynpm --republish
	deploy/publish mypypi --republish
	$(MAKE) smoke-deployed

SITE_DIR ?= .work/site
BUCKET ?=
LOCAL_STATE ?= .work/r2-local

sync: ## Sync a published site dir to its R2 bucket (SITE_DIR= BUCKET=, LOCAL=1 for the local simulation)
	@test -n "$(BUCKET)" || (echo "usage: make sync SITE_DIR=<dir> BUCKET=<name> [LOCAL=1]"; exit 1)
	@if [ -n "$(LOCAL)" ]; then CF_LOCAL_STATE="$(LOCAL_STATE)" deploy/r2-sync.sh $(SITE_DIR) $(BUCKET); \
	else deploy/r2-sync.sh $(SITE_DIR) $(BUCKET); fi

SERVE_PORT ?= 8000

serve: ## Serve a published site dir locally (SITE_DIR= [SERVE_PORT=])
	cargo run -q -p stuff-server -- --bind 127.0.0.1:$(SERVE_PORT) --route /=$(SITE_DIR)

test-sync: ## Check r2-sync pagination and idempotency on the local R2 simulation ([SYNC_OBJECTS=1100])
	TMPDIR="$${TMPDIR:-.work/tmp}" deploy/test-r2-sync.sh $(or $(SYNC_OBJECTS),1100)

test-publish-lock: ## Check the cross-repo publish lock serializes writers ([WAITERS=4])
	TMPDIR="$${TMPDIR:-.work/tmp}" deploy/test-publish-lock.sh $(or $(WAITERS),4)

test-r2: ## Check multipart SigV4, pagination, and conditional promotion without network access
	TMPDIR="$${TMPDIR:-.work/tmp}" deploy/test-r2-sync.sh $(or $(SYNC_OBJECTS),1100)
	deploy/test-r2-pull.sh
	python3 deploy/test-r2-multipart.py
	python3 deploy/test-r2-plan.py
	python3 deploy/test-r2-promote.py

test-r2-live: ## Opt-in live R2 multipart acceptance (R2_ACCEPTANCE_BUCKET=... and R2_S3_*)
	python3 deploy/test-r2-multipart-live.py

FLEET_OUT ?= .work/fleet

fleet-ingest: ## Ingest fleet lists into local per-registry sites, no R2 (FLEET_OUT= CARGO_LIST= NPM_LIST= PYPI_LIST=)
	deploy/fleet-ingest.sh $(FLEET_OUT) $(if $(CARGO_LIST),--cargo $(CARGO_LIST)) $(if $(NPM_LIST),--npm $(NPM_LIST)) $(if $(PYPI_LIST),--pypi $(PYPI_LIST))
