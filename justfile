default: check

check:
    uv run --no-project python scripts/check.py --mode local

quick:
    uv run --no-project python scripts/check.py --mode quick

test:
    cargo test

fmt:
    cargo fmt

lint:
    cargo clippy --all-targets -- -D warnings

scan:
    cargo run --quiet -- scan
