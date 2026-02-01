# Plasmite task runner (just).

set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

default:
	@just --list

fmt:
	cargo fmt --all

clippy:
	cargo clippy --all-targets -- -D warnings

test:
	cargo test

ci: fmt clippy test

.scratch:
	mkdir -p .scratch

audit-db: .scratch
	if [ -d .scratch/advisory-db/.git ]; then \
	  git -C .scratch/advisory-db pull --ff-only; \
	else \
	  git clone https://github.com/RustSec/advisory-db.git .scratch/advisory-db; \
	fi

audit: audit-db
	cargo audit --db .scratch/advisory-db --no-fetch

bench:
	cargo build --release --example plasmite-bench
	./target/release/examples/plasmite-bench

bench-json:
	cargo build --release --example plasmite-bench
	./target/release/examples/plasmite-bench --format json > bench.json

clean:
	cargo clean

release-check: ci audit
