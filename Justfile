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

abi:
	cargo build --lib
	@ls -1 target/debug/libplasmite.* 2>/dev/null || true

abi-release:
	cargo build --release --lib
	@ls -1 target/release/libplasmite.* 2>/dev/null || true

abi-test: abi
	cargo test abi_smoke

scratch:
	mkdir -p .scratch

audit-db: scratch
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

install:
	cargo install --path . --locked

clean:
	cargo clean

release-check: ci audit
