.PHONY: test lint deps dashboard-ts

test:
	@uv run --group dev --quiet pytest tests/

lint:
	@uv run --group dev --quiet ruff check src tests
	@uv run --group dev --quiet ty check

PORT ?= 8765
PNPM ?= pnpm dlx pnpm@12.18.0

deps:
	@uv run repomod deps render
	@exec srv -p $(PORT) .work/dependencies/dashboard

dashboard-ts:
	@$(PNPM) install --frozen-lockfile
	@$(PNPM) run check
	@$(PNPM) run lint
	@$(PNPM) run build
	@cp node_modules/@laputa-systems/ui-dashboard/dist/styles.css node_modules/@laputa-systems/ui-dashboard/dist/tokens.css src/repomod/dependencies/assets/ui-dashboard/
	@git diff --exit-code -- src/repomod/dependencies/assets/app.js src/repomod/dependencies/assets/search.js src/repomod/dependencies/assets/ui-dashboard
