.PHONY: test lint

test:
	@uv run --group dev --quiet pytest tests/

lint:
	@uv run --group dev --quiet ruff check src tests
	@uv run --group dev --quiet ty check
