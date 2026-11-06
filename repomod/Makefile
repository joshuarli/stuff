.PHONY: test lint deps

test:
	@uv run --group dev --quiet pytest tests/

lint:
	@uv run --group dev --quiet ruff check src tests
	@uv run --group dev --quiet ty check

PORT ?= 8765

deps:
	@uv run repomod deps render
	@exec srv -p $(PORT) .work/dependencies/dashboard
