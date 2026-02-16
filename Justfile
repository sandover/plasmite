# Plasmite task runner (just).

set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# Show available recipes.
default:
	@just --list

# Format all Rust code.
fmt:
	cargo fmt --all

# Lint Rust code with warnings denied.
clippy:
	cargo clippy --all-targets -- -D warnings

# Run Rust test suites.
test:
	cargo test

# Lane A: deterministic fast hardening checks for local iteration + PR CI.
# Keep runtime bounded and avoid flaky timing-sensitive scenarios.
hardening-fast: test
	@echo "hardening-fast complete"

# Lane B: broader deterministic hardening checks for full/main CI.
# This lane is intentionally broader than Lane A but still release-agnostic.
hardening-broad: conformance-all cross-artifact-smoke
	@echo "hardening-broad complete"

# Verify version alignment across release surfaces.
check-version-alignment:
	./scripts/check-version-alignment.sh

# Run Go bindings tests with repo-local caches.
bindings-go-test:
	cargo build -p plasmite
	mkdir -p tmp/go-cache tmp/go-tmp
	cd bindings/go && GOCACHE="$(pwd)/../../tmp/go-cache" GOTMPDIR="$(pwd)/../../tmp/go-tmp" PLASMITE_LIB_DIR="$(pwd)/../../target/debug" PKG_CONFIG="/usr/bin/true" CGO_CFLAGS="-I$(pwd)/../../include" CGO_LDFLAGS="-L$(pwd)/../../target/debug" go test ./...

# Run Python bindings unit tests.
bindings-python-test:
	cargo build -p plasmite
	cd bindings/python && PLASMITE_LIB_DIR="$(pwd)/../../target/debug" PLASMITE_BIN="$(pwd)/../../target/debug/plasmite" python3 -m unittest discover -s tests

# Run Node bindings tests.
bindings-node-test:
	cargo build -p plasmite
	cd bindings/node && PLASMITE_LIB_DIR="$(pwd)/../../target/debug" npm test
	bash scripts/node_pack_smoke.sh
	bash scripts/node_remote_only_smoke.sh

# Run Node bindings type checks.
bindings-node-typecheck:
	cd bindings/node && npm run typecheck

# Run all language bindings tests and checks.
bindings-test: bindings-go-test bindings-python-test bindings-node-test bindings-node-typecheck

# Fast local CI parity gate used during iteration.
ci-fast: fmt clippy hardening-fast check-version-alignment bindings-node-typecheck

# Full CI parity gate including ABI/conformance/cross-artifact checks.
ci-full: fmt clippy hardening-fast check-version-alignment abi-smoke hardening-broad bindings-node-typecheck

# Alias for full CI gate.
ci: ci-full

# Build shared library artifacts for local ABI usage.
abi:
	cargo build --lib
	@ls -1 target/debug/libplasmite.* 2>/dev/null || true

# Build release shared library artifacts.
abi-release:
	cargo build --release --lib
	@ls -1 target/release/libplasmite.* 2>/dev/null || true

# Build ABI artifacts and run ABI smoke unit.
abi-test: abi
	cargo test abi_smoke

# Run ABI smoke script against built artifacts.
abi-smoke: abi
	./scripts/abi_smoke.sh

# Run full conformance suite.
conformance-all:
	./scripts/conformance_all.sh

# Verify behavior across published artifact boundaries.
cross-artifact-smoke:
	./scripts/cross_artifact_smoke.sh

# Ensure scratch workspace exists.
scratch:
	mkdir -p .scratch

# Refresh or clone the RustSec advisory DB into repo-local scratch space.
audit-db: scratch
	if [ -d .scratch/advisory-db/.git ]; then \
	  git -C .scratch/advisory-db pull --ff-only; \
	else \
	  git clone https://github.com/RustSec/advisory-db.git .scratch/advisory-db; \
	fi

# Run cargo-audit against the locally pinned advisory DB.
audit: audit-db
	cargo audit --db .scratch/advisory-db --no-fetch --ignore yanked

# Build and execute the benchmark example in release mode.
bench:
	cargo build --release --example plasmite-bench
	./target/release/examples/plasmite-bench

# Emit benchmark output as JSON for tooling/analysis.
bench-json:
	cargo build --release --example plasmite-bench
	./target/release/examples/plasmite-bench --format json > bench.json

# Install plasmite from this working tree.
install:
	cargo install --path . --locked

# Remove build artifacts.
clean:
	cargo clean

# --- Dev server management ---
# All dev state lives under /tmp/plasmite-dev/.
# serve-dev is idempotent: it kills any previous server before starting a fresh one.

_dev_dir := "/tmp/plasmite-dev"
_dev_port := "9009"
_dev_bind := "127.0.0.1:" + _dev_port

# Build, seed test data, and start a dev server on :9009 (returns immediately).
serve-dev: _serve-kill
	cargo build --bin plasmite
	mkdir -p {{_dev_dir}}/pools
	@# Seed a demo pool if it doesn't already exist
	if [ ! -f {{_dev_dir}}/pools/demo.plasmite ]; then \
	  ./target/debug/plasmite --dir {{_dev_dir}}/pools pool create demo --size 1M > /dev/null; \
	  ./target/debug/plasmite --dir {{_dev_dir}}/pools feed demo --tag deploy '{"service":"api","version":"1.0"}' > /dev/null; \
	  ./target/debug/plasmite --dir {{_dev_dir}}/pools feed demo --tag metric '{"cpu":12.3,"rps":4200}' > /dev/null; \
	  echo "serve-dev: seeded demo pool with 2 messages"; \
	fi
	@nohup ./target/debug/plasmite --dir {{_dev_dir}}/pools serve --bind {{_dev_bind}} \
	  > {{_dev_dir}}/serve.log 2>&1 & echo $! > {{_dev_dir}}/serve.pid
	@sleep 0.5
	@# Verify it actually started
	@if kill -0 $(cat {{_dev_dir}}/serve.pid) 2>/dev/null; then \
	  echo "serve-dev: server running (pid $(cat {{_dev_dir}}/serve.pid))"; \
	  echo "serve-dev: http://{{_dev_bind}}/ui"; \
	  echo "serve-dev: log at {{_dev_dir}}/serve.log"; \
	  echo "serve-dev: sandbox-safe one-shot: just serve-with '<command>'"; \
	  echo "serve-dev: stop with 'just serve-stop'"; \
	else \
	  echo "serve-dev: ERROR — server exited immediately. Check {{_dev_dir}}/serve.log"; \
	  cat {{_dev_dir}}/serve.log; \
	  exit 1; \
	fi

# Run a command while hosting a temporary dev server (sandbox-safe).
# Example: just serve-with "agent-browser open http://127.0.0.1:9009/ui/pools/demo"
serve-with cmd: _serve-kill
	cargo build --bin plasmite
	mkdir -p {{_dev_dir}}/pools
	@if [ ! -f {{_dev_dir}}/pools/demo.plasmite ]; then \
	  ./target/debug/plasmite --dir {{_dev_dir}}/pools pool create demo --size 1M > /dev/null; \
	  ./target/debug/plasmite --dir {{_dev_dir}}/pools feed demo --tag deploy '{"service":"api","version":"1.0"}' > /dev/null; \
	  ./target/debug/plasmite --dir {{_dev_dir}}/pools feed demo --tag metric '{"cpu":12.3,"rps":4200}' > /dev/null; \
	  echo "serve-with: seeded demo pool with 2 messages"; \
	fi
	@set -euo pipefail; \
	  ./target/debug/plasmite --dir {{_dev_dir}}/pools serve --bind {{_dev_bind}} > {{_dev_dir}}/serve.log 2>&1 & \
	  pid=$$!; \
	  trap 'kill $$pid 2>/dev/null || true' EXIT; \
	  sleep 0.5; \
	  if ! kill -0 $$pid 2>/dev/null; then \
	    echo "serve-with: ERROR — server exited immediately. Check {{_dev_dir}}/serve.log"; \
	    cat {{_dev_dir}}/serve.log; \
	    exit 1; \
	  fi; \
	  {{cmd}}

# Start dev server with bearer auth enabled.
serve-dev-auth token="devtoken": _serve-kill
	cargo build --bin plasmite
	mkdir -p {{_dev_dir}}/pools
	@if [ ! -f {{_dev_dir}}/pools/demo.plasmite ]; then \
	  ./target/debug/plasmite --dir {{_dev_dir}}/pools pool create demo --size 1M > /dev/null; \
	  ./target/debug/plasmite --dir {{_dev_dir}}/pools feed demo --tag deploy '{"service":"api","version":"1.0"}' > /dev/null; \
	  echo "serve-dev-auth: seeded demo pool with 1 message"; \
	fi
	@nohup ./target/debug/plasmite --dir {{_dev_dir}}/pools serve --bind {{_dev_bind}} --token {{token}} \
	  > {{_dev_dir}}/serve.log 2>&1 & echo $! > {{_dev_dir}}/serve.pid
	@sleep 0.5
	@if kill -0 $(cat {{_dev_dir}}/serve.pid) 2>/dev/null; then \
	  echo "serve-dev-auth: server running (pid $(cat {{_dev_dir}}/serve.pid))"; \
	  echo "serve-dev-auth: http://{{_dev_bind}}/ui?token={{token}}"; \
	  echo "serve-dev-auth: auth required — token: {{token}}"; \
	  echo "serve-dev-auth: log at {{_dev_dir}}/serve.log"; \
	  echo "serve-dev-auth: stop with 'just serve-stop'"; \
	else \
	  echo "serve-dev-auth: ERROR — server exited immediately. Check {{_dev_dir}}/serve.log"; \
	  cat {{_dev_dir}}/serve.log; \
	  exit 1; \
	fi

# Show status of the dev server.
serve-status:
	@if [ -f {{_dev_dir}}/serve.pid ] && kill -0 $(cat {{_dev_dir}}/serve.pid) 2>/dev/null; then \
	  echo "serve-status: running (pid $(cat {{_dev_dir}}/serve.pid))"; \
	  echo "serve-status: http://{{_dev_bind}}/ui"; \
	  echo "serve-status: log at {{_dev_dir}}/serve.log"; \
	else \
	  echo "serve-status: not running"; \
	fi

# Stop the dev server.
serve-stop: _serve-kill
	@echo "serve-stop: done"

# Tail the dev server log.
serve-log:
	@if [ -f {{_dev_dir}}/serve.log ]; then tail -40 {{_dev_dir}}/serve.log; else echo "serve-log: no log file"; fi

# Internal: kill any existing dev server.
_serve-kill:
	@if [ -f {{_dev_dir}}/serve.pid ]; then \
	  pid=$(cat {{_dev_dir}}/serve.pid); \
	  if kill -0 $pid 2>/dev/null; then \
	    kill $pid 2>/dev/null || true; \
	    echo "serve: killed previous server (pid $pid)"; \
	    sleep 0.3; \
	  fi; \
	  rm -f {{_dev_dir}}/serve.pid; \
	fi
	@# Also clean up orphan listeners on the dev port in case pidfile state was lost/stale.
	@for pid in $(lsof -nP -tiTCP:{{_dev_port}} -sTCP:LISTEN 2>/dev/null || true); do \
	  if kill -0 $pid 2>/dev/null; then \
	    kill $pid 2>/dev/null || true; \
	    echo "serve: killed orphan listener on :{{_dev_port}} (pid $pid)"; \
	  fi; \
	done

# Run full release readiness checks.
release-check: ci audit
