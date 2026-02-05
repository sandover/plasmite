<!--
Purpose: Document the project’s test suites and how to run them locally.
Exports: N/A (documentation).
Role: Contributor guide for validation and confidence-building.
Invariants: Commands in this doc must be runnable from a clean checkout.
-->

# Testing

This repo uses Rust’s built-in test harness (`cargo test`) plus a small set of
integration tests that exec the `plasmite` CLI binary.

## Quick start

Run everything (unit + integration):

```bash
cargo test
```

## Test suites

### Unit tests (`src/`)

Run library unit tests (core correctness + invariants):

```bash
cargo test --lib
```

Run binary unit tests (CLI parsing helpers, etc.):

```bash
cargo test --bin plasmite
```

What they cover (high level):
- Ring/frame encoding + validation (`src/core/frame.rs`)
- Pure append planning invariants (`src/core/plan.rs`)
- Pool I/O + mmap + locking behavior (`src/core/pool.rs`)
- Cursor iteration / overwrite safety (`src/core/cursor.rs`)
- Error kinds + exit-code mapping (`src/core/error.rs`)
- Canonical Lite3 payload validation (`src/core/lite3/`)

### Integration tests (`tests/`)

Run the end-to-end CLI contract tests:

```bash
cargo test --test cli_integration
```

What they cover (high level):
- `pool create` / `poke` / `get` / `peek` minimal flows
- JSON-on-stdout success output shapes
- JSON-on-stderr error output shapes + exit codes
- Streaming JSON stdin behavior for `poke`
- `peek` watch behavior (bounded waits)

Run the multi-process lock smoke test:

```bash
cargo test --test lock_smoke
```

What it covers:
- Spawns multiple `plasmite poke` processes against one pool
- Asserts writes are serialized (bounds reflect all writes)

## Security/advisory checks

The CI runs RustSec audits via `cargo audit`. To run locally (keeping the
advisory database under `.scratch/` to avoid `~/.cargo` write permissions):

```bash
cargo install cargo-audit --locked
mkdir -p .scratch
if [ -d .scratch/advisory-db/.git ]; then
  git -C .scratch/advisory-db pull --ff-only
else
  git clone https://github.com/RustSec/advisory-db.git .scratch/advisory-db
fi
cargo audit --db .scratch/advisory-db --no-fetch
```

## Conformance suite

The conformance suite validates that all language bindings behave consistently.
See `conformance/README.md` for the full manifest format.

Run the Rust conformance runner:

```bash
cargo run --bin plasmite-conformance -- conformance/sample-v0.json
```

Run Go conformance:

```bash
cd bindings/go
CGO_LDFLAGS="-L$(pwd)/../../target/debug" go run ./cmd/plasmite-conformance ../../conformance/sample-v0.json
```

## Binding tests

### Go

```bash
cd bindings/go
cargo build -p plasmite  # build libplasmite first
CGO_LDFLAGS="-L$(pwd)/../../target/debug" go test ./...
```

### Python

```bash
cd bindings/python
cargo build -p plasmite
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" python -m unittest
```

### Node.js

```bash
cd bindings/node
cargo build -p plasmite
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" npm test
```

## Notes

- Toolchain: pinned in `rust-toolchain.toml` (includes `clippy` + `rustfmt`).
- C toolchain: builds vendor Lite3 via `build.rs` (you need a working C compiler).
- Failure artifacts: some debug-only validation paths write snapshots to `.scratch/`
  (safe to delete).

## Benchmarks (not tests)

The repo includes a lightweight benchmark harness:

```bash
cargo run --example plasmite-bench -- --help
```
