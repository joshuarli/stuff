.PHONY: test lint deps dashboard-ts bench bench-cli

test:
	@uv run --group dev --quiet pytest tests/

lint:
	@uv run --group dev --quiet ruff check src tests bench
	@uv run --group dev --quiet ty check

PORT ?= 8765
PNPM ?= pnpm dlx pnpm@12.18.0
RUNS ?= 3

deps:
	@uv run repomod deps render --status $(if $(filter 1,$(PRIVATE)),--include-private)
	@exec srv -p $(PORT) .work/dependencies/dashboard

bench:
	@uv run python bench/deps_perf.py phases --mode both --runs $(RUNS)

bench-cli:
	@uv run python bench/deps_perf.py workflow --mode both --runs $(RUNS)

dashboard-ts:
	@$(PNPM) install --frozen-lockfile
	@$(PNPM) run check
	@$(PNPM) run lint
	@$(PNPM) run build
	@cp node_modules/@laputa-systems/ui-dashboard/dist/styles.css node_modules/@laputa-systems/ui-dashboard/dist/tokens.css src/repomod/dependencies/assets/ui-dashboard/
	@git diff --exit-code -- src/repomod/dependencies/assets/app.js src/repomod/dependencies/assets/search.js src/repomod/dependencies/assets/ui-dashboard
