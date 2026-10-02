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

# Verify the pinned Lite3 snapshot without accessing the network.
verify-lite3:
	./scripts/verify-lite3.sh

# Replace the curated Lite3 snapshot with an exact upstream commit.
update-lite3 commit:
	./scripts/update-lite3.sh "{{commit}}"

# Query upstream and fail when Lite3 main has moved beyond the pin.
check-lite3-upstream:
	./scripts/check-lite3-upstream.sh

# Run focused Lite3 tests with C AddressSanitizer and UndefinedBehaviorSanitizer instrumentation (Linux only).
test-lite3-sanitizers:
	./scripts/test-lite3-sanitizers.sh

cookbook-smoke:
	bash scripts/cookbook_smoke.sh
	@echo "cookbook-smoke complete"

# Verify version alignment across release surfaces.
check-version-alignment:
	./scripts/check-version-alignment.sh

# Keep the release matrix and documented support tiers aligned.
check-release-targets:
	./scripts/test_release_targets.sh
	./scripts/validate_distribution_targets.sh
	bash scripts/test_post_release_delivery_smoke.sh

# Run Go bindings tests with repo-local caches.
bindings-go-test:
	cargo build -p plasmite
	mkdir -p "${GOCACHE:-tmp/go-cache}" tmp/go-tmp
	cd bindings/go && GOCACHE="${GOCACHE:-$(pwd)/../../tmp/go-cache}" GOTMPDIR="$(pwd)/../../tmp/go-tmp" PLASMITE_LIB_DIR="$(pwd)/../../target/debug" LD_LIBRARY_PATH="$(pwd)/../../target/debug:${LD_LIBRARY_PATH:-}" DYLD_LIBRARY_PATH="$(pwd)/../../target/debug:${DYLD_LIBRARY_PATH:-}" PKG_CONFIG="/usr/bin/true" CGO_CFLAGS="-I$(pwd)/../../include" CGO_LDFLAGS="-L$(pwd)/../../target/debug" go test ./...

# Run Go API contract tests without CGO.
bindings-go-contract-test:
	cd bindings/go && CGO_ENABLED=0 go test ./api/...

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

# Check browser sequence precision, bounded history, and recovery behavior.
ui-smoke:
	node --test scripts/ui_smoke.cjs

# Check benchmark OAuth cleanup without live credentials or a server.
benchmark-harness-test:
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_bench_transport_comparison.py

# Core, deterministic checks for every local change and pull request.
check: _trim-target fmt clippy test benchmark-harness-test check-version-alignment check-release-targets verify-lite3

# Cargo never removes builds that newer ones replace, so target/ grows without bound.
# A clean build is about 1 GB and takes under a minute, so past 2 GB start over.
_trim-target:
	@if [ -d target ] && [ "$(du -sk target | cut -f1)" -gt 2097152 ]; then \
	  echo "target/ is over 2 GB; running cargo clean"; \
	  cargo clean; \
	fi

# Cross-language and artifact checks. Requires Go, Node, Python, and uv.
# `bindings-test` includes node pack and remote-only smoke tests.
integration: cookbook-smoke abi-smoke conformance-all cross-artifact-smoke bindings-test ui-smoke

# Canonical release candidate gate before merging, tagging, or publishing.
release-gate: check integration abi-release
	bash scripts/python_wheel_smoke.sh

# Build shared library artifacts for local ABI usage.
abi:
	cargo build --lib
	@ls -1 target/debug/libplasmite.* 2>/dev/null || true

# Build release shared library artifacts.
abi-release:
	cargo build --release
	@ls -1 target/release/libplasmite.* 2>/dev/null || true

# Build ABI artifacts and run ABI smoke unit.
abi-test: abi
	cargo test abi_smoke

# Run ABI smoke script against built artifacts.
abi-smoke: abi
	./scripts/abi_smoke.sh
	./scripts/abi_adversarial.sh

# Run full conformance suite.
conformance-all:
	./scripts/conformance_all.sh

# Verify behavior across published artifact boundaries.
cross-artifact-smoke:
	./scripts/cross_artifact_smoke.sh

# Build a release-style SDK tarball from source (for C/libplasmite consumers).
# Defaults target Linux x86_64; override for other target triples.
sdk-from-source target="x86_64-unknown-linux-gnu":
	version="$(awk -F '\"' '/^version = \"/ {print $2; exit}' Cargo.toml)"; \
	if [[ -z "$version" ]]; then \
	  echo "failed to detect version from Cargo.toml" >&2; \
	  exit 1; \
	fi; \
	platform="$(./scripts/release_target_field.sh "{{target}}" sdk_platform)"; \
	./scripts/build_release_artifacts.sh "{{target}}" --static; \
	./scripts/package_release_sdk.sh "{{target}}" "$version"; \
	./scripts/cross_artifact_smoke.sh "dist/plasmite_${version}_${platform}.tar.gz" "{{target}}"; \
	echo "sdk-from-source complete: dist/plasmite_${version}_${platform}.tar.gz"

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
# Demo pools, logs, and the tracked PID live under /tmp/plasmite-dev/.
# serve-dev replaces only its tracked server after verifying the process identity.
# The HTTPS listener uses an ephemeral loopback port; serve status shows its address.

_dev_dir := "/tmp/plasmite-dev"
_dev_port := "9009"
_dev_bind := "127.0.0.1:" + _dev_port
_dev_bin := "./target/debug/plasmite"
_dev_pool_dir := _dev_dir + "/pools"
_dev_log := _dev_dir + "/serve.log"
_dev_pid := _dev_dir + "/serve.pid"

# Build, seed test data, and start a dev server on :9009 (returns immediately).
serve-dev: _serve-kill
	cargo build --bin plasmite
	bash scripts/serve_dev.sh seed-demo {{_dev_bin}} {{_dev_pool_dir}} serve-dev
	bash scripts/serve_dev.sh start-detached {{_dev_bin}} {{_dev_pool_dir}} {{_dev_bind}} {{_dev_log}} {{_dev_pid}} serve-dev
	@echo "serve-dev: server running (pid $(cat {{_dev_pid}}))"
	@echo "serve-dev: http://{{_dev_bind}}/ui"
	@echo "serve-dev: log at {{_dev_log}}"
	@echo "serve-dev: HTTPS access: {{_dev_bin}} --dir {{_dev_pool_dir}} access invite --name <client>"
	@echo "serve-dev: find its HTTPS address with {{_dev_bin}} serve status"
	@echo "serve-dev: sandbox-safe one-shot: just serve-with '<command>'"
	@echo "serve-dev: stop with 'just serve-stop'"

# Run a command while hosting a temporary dev server (sandbox-safe).
# Example: just serve-with "agent-browser open http://127.0.0.1:9009/ui/pools/demo"
serve-with cmd: _serve-kill
	cargo build --bin plasmite
	bash scripts/serve_dev.sh seed-demo {{_dev_bin}} {{_dev_pool_dir}} serve-with
	bash scripts/serve_dev.sh run-with {{_dev_bin}} {{_dev_pool_dir}} {{_dev_bind}} {{_dev_log}} serve-with "{{cmd}}"

# Show status of the dev server.
serve-status:
	bash scripts/serve_dev.sh status {{_dev_bin}} {{_dev_pool_dir}} {{_dev_bind}} {{_dev_pid}} {{_dev_log}}

# Stop the dev server.
serve-stop: _serve-kill
	@echo "serve-stop: done"

# Tail the dev server log.
serve-log:
	@if [ -f {{_dev_log}} ]; then tail -40 {{_dev_log}}; else echo "serve-log: no log file"; fi

# Internal: stop only the tracked dev process; leave unrelated listeners alone.
_serve-kill:
	bash scripts/serve_dev.sh stop {{_dev_bin}} {{_dev_pool_dir}} {{_dev_bind}} {{_dev_pid}} serve

# Run full release readiness checks.
release-check: release-gate audit
