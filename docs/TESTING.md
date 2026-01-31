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
- `peek --follow` behavior (bounded timeouts)

Run the multi-process lock smoke test:

```bash
cargo test --test lock_smoke
```

What it covers:
- Spawns multiple `plasmite poke` processes against one pool
- Asserts writes are serialized (bounds reflect all writes)

## Security/advisory checks

The CI runs RustSec audits via `cargo audit`. To run locally:

```bash
cargo install cargo-audit --locked
cargo audit
```

## Notes

- Toolchain: pinned in `rust-toolchain.toml` (includes `clippy` + `rustfmt`).
- C toolchain: builds vendor Lite3 via `build.rs` (you need a working C compiler).
- Failure artifacts: some debug-only validation paths write snapshots to `.scratch/`
  (safe to delete).

## Benchmarks (not tests)

The CLI includes a lightweight benchmark harness:

```bash
cargo run -- bench --help
```
