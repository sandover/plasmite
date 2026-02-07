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

bindings-go-test:
	cargo build -p plasmite
	mkdir -p tmp/go-cache tmp/go-tmp
	cd bindings/go && GOCACHE="$(pwd)/../../tmp/go-cache" GOTMPDIR="$(pwd)/../../tmp/go-tmp" CGO_LDFLAGS="-L$(pwd)/../../target/debug" go test ./...

bindings-python-test:
	cargo build -p plasmite
	cd bindings/python && PLASMITE_LIB_DIR="$(pwd)/../../target/debug" PLASMITE_BIN="$(pwd)/../../target/debug/plasmite" python3 -m unittest discover -s tests

bindings-node-test:
	cargo build -p plasmite
	cd bindings/node && PLASMITE_LIB_DIR="$(pwd)/../../target/debug" npm test

bindings-node-typecheck:
	cd bindings/node && npm run typecheck

bindings-test: bindings-go-test bindings-python-test bindings-node-test bindings-node-typecheck

ci: fmt clippy test abi-smoke conformance-all cross-artifact-smoke bindings-node-typecheck

abi:
	cargo build --lib
	@ls -1 target/debug/libplasmite.* 2>/dev/null || true

abi-release:
	cargo build --release --lib
	@ls -1 target/release/libplasmite.* 2>/dev/null || true

abi-test: abi
	cargo test abi_smoke

abi-smoke: abi
	./scripts/abi_smoke.sh

conformance-all:
	./scripts/conformance_all.sh

cross-artifact-smoke:
	./scripts/cross_artifact_smoke.sh

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
