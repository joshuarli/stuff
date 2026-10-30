.PHONY: help build test clippy smoke pipecheck worker-build worker-check worker-deploy sync serve

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

pipecheck: ## Prove one registry's publication never touches another's output
	deploy/check-pipeline-independence.sh

WORKER_DIR ?= crates/stuff-worker

worker-build: ## Compile the worker wasm glue into crates/stuff-worker/build
	cd $(WORKER_DIR) && npm run build:wasm

worker-check: ## Validate the worker build and deploy plan without credentials
	cd $(WORKER_DIR) && cf deploy --dry-run

worker-deploy: ## Deploy the worker, provision R2 buckets, attach custom domains
	cd $(WORKER_DIR) && cf deploy

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
