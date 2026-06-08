# YAAML development tasks

default: check

check: lint typecheck test

lint:
    uv run ruff check src/ tests/
    uv run ruff format --check src/ tests/

typecheck:
    uv run mypy src/

test:
    uv run pytest tests/ -v

fmt:
    uv run ruff format src/ tests/
    uv run ruff check --fix src/ tests/
