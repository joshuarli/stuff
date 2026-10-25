.PHONY: sync test lint

sync:
	@uv run repomod sync

test:
	@uv run --group dev --quiet pytest tests/

lint:
	@uv run --group dev --quiet ruff check src tests
	@uv run --group dev --quiet ty check
