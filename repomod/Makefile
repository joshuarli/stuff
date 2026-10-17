.PHONY: sync test

sync:
	@uv run repomod sync

test:
	@uv run --group dev --quiet pytest tests/
