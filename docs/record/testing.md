# Testing

This repo uses Rust’s built-in test harness (`cargo test`) plus a small set of
integration tests that exec the `plasmite` CLI binary.

## Quick start

Run everything (unit + integration):

```bash
cargo test
```

## Execution lanes (pragmatic policy)

Plasmite uses three test lanes so we can add risk coverage without introducing
release complexity or fragile gating.

- **Lane A (fast, deterministic):** local MacBook + pull-request CI path.
  - Command: `just hardening-fast`
  - Included by: `just ci-fast` (and therefore PR CI)
  - Intended for deterministic checks with bounded runtime overhead.
  - Includes `bash scripts/cookbook_smoke.sh` for golden cookbook coverage.
- **Lane B (broad, deterministic):** main/scheduled full CI path.
  - Command: `just hardening-broad`
  - Included by: `just ci` / `just ci-full`
  - Intended for broader deterministic compatibility checks.
- **Lane C (manual/on-demand):** developer-invoked deep checks only.
  - Not part of required CI or release-publish automation.

Current and planned hardening work from epic `I5PCAT` maps to lanes as follows:

- Gap 1 (seeded parser/frame robustness): Lane A for core deterministic cases,
  Lane B for broader deterministic corpus checks.
- Gap 2 (fault-matrix I/O/process failures): Lane A for stable black-box
  failures, Lane B for broader matrix coverage.
- Gap 4 (remote adversarial stream lifecycle): Lane A for deterministic
  reconnect/cancel boundary checks, Lane B for expanded matrix.
- Gap 5 (auth/CORS abuse-negative matrix): Lane A for critical malformed-input
  rejects, Lane B for broader edge matrix.
- Cookbook executable examples: Lane A for golden local examples; Lane B for
  expanded deterministic docs-smoke coverage.
  - Golden sections in `docs/cookbook.md`: CI Gate, Live Build Progress,
    Multi-Writer Event Bus, Replay & Debug, Remote Pool Access.

### Cookbook smoke checks

Run:

```bash
bash scripts/cookbook_smoke.sh
```

The script validates a focused set of end-to-end cookbook examples using only local
and loopback operations:

- CI Gate
- Live Build Progress
- Multi-Writer Event Bus
- Replay & Debug
- Remote Pool Access

Policy constraints:

- Prefer deterministic tests; avoid flaky timing assumptions.
- No new release workflow stages or release-publish gates for hardening tests.
- Keep checks runnable on local macOS and standard GitHub CI runners.

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
- `pool create` / `feed` / `fetch` / `follow` minimal flows
- JSON-on-stdout success output shapes
- JSON-on-stderr error output shapes + exit codes
- Streaming JSON stdin behavior for `feed`
- `follow` behavior (bounded waits)

Run the multi-process lock smoke test:

```bash
cargo test --test lock_smoke
```

What it covers:
- Spawns multiple `plasmite feed` processes against one pool
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

The recommended way to run all binding tests is via the Justfile:

```bash
just bindings-test
```

### Go

```bash
just bindings-go-test
```

CI also enforces a no-CGO API-contract validation:

```bash
cd bindings/go
CGO_ENABLED=0 go test ./api/...
```

Or manually:

```bash
cd bindings/go
cargo build -p plasmite  # build libplasmite first
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" \
  PKG_CONFIG="/usr/bin/true" \
  CGO_CFLAGS="-I$(pwd)/../../include" \
  CGO_LDFLAGS="-L$(pwd)/../../target/debug" \
  go test ./...
```

### Python

```bash
just bindings-python-test
```

Or manually:

```bash
cd bindings/python
cargo build -p plasmite
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" python3 -m unittest discover -s tests
```

### Node.js

```bash
just bindings-node-test
```

Or manually:

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
