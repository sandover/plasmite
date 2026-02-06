#!/usr/bin/env bash
# Purpose: Run all language conformance suites against a single, freshly built artifact set.
# Key exports: N/A (script entry point).
# Role: CI/local parity gate to prevent stale lib/bin drift across Rust/Go/Node/Python.
# Invariants: Builds CLI + cdylib + staticlib before running any conformance runner.
# Invariants: Uses workspace-local caches to avoid host-global state drift.
# Notes: Keep this script deterministic and side-effect scoped to .scratch/ and target/.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIB_DIR="$ROOT/target/debug"
MANIFESTS=(
  "sample-v0.json"
  "negative-v0.json"
  "multiprocess-v0.json"
  "pool-admin-v0.json"
)

mkdir -p "$ROOT/.scratch/go-build"

if [[ "${RUNNER_OS:-}" == "macOS" || "$(uname -s)" == "Darwin" ]]; then
  export DYLD_LIBRARY_PATH="$LIB_DIR${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
else
  export LD_LIBRARY_PATH="$LIB_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
export LIBRARY_PATH="$LIB_DIR${LIBRARY_PATH:+:$LIBRARY_PATH}"
export PLASMITE_LIB_DIR="$LIB_DIR"
export PLASMITE_BIN="$LIB_DIR/plasmite"
export GOCACHE="$ROOT/.scratch/go-build"

echo "[conformance] building artifacts"
cargo build --bin plasmite
cargo rustc --lib --crate-type=cdylib
cargo rustc --lib --crate-type=staticlib

echo "[conformance] rust runner"
for manifest in "${MANIFESTS[@]}"; do
  cargo run --bin plasmite-conformance -- "$ROOT/conformance/$manifest"
done

echo "[conformance] go runner"
(
  cd "$ROOT/bindings/go"
  for manifest in "${MANIFESTS[@]}"; do
    CGO_LDFLAGS="-L$LIB_DIR" go run ./cmd/plasmite-conformance "$ROOT/conformance/$manifest"
  done
)

echo "[conformance] node runner/tests"
(
  cd "$ROOT/bindings/node"
  if [[ -d node_modules ]]; then
    echo "[conformance] using existing node_modules"
  else
    npm install --no-package-lock
  fi
  npm test
)

echo "[conformance] python runner/tests"
(
  cd "$ROOT/bindings/python"
  PYTHONPATH="$ROOT/bindings/python" python3 -m unittest discover -s tests -p "test_*.py"
)

echo "[conformance] complete"
