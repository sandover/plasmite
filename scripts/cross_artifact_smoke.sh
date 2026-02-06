#!/usr/bin/env bash
# Purpose: Verify pool-format compatibility across CLI and language bindings.
# Key exports: N/A (script entry point).
# Role: Guardrail smoke test that catches stale artifact/version mismatch early.
# Invariants: Uses only workspace-local temp state under .scratch/.
# Invariants: Asserts all created pools carry the current format version.
# Notes: Assumes target/debug artifacts are already built.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIB_DIR="$ROOT/target/debug"
WORKDIR="$(mktemp -d "$ROOT/.scratch/cross-artifact.XXXXXX")"

if [[ "${RUNNER_OS:-}" == "macOS" || "$(uname -s)" == "Darwin" ]]; then
  export DYLD_LIBRARY_PATH="$LIB_DIR${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
else
  export LD_LIBRARY_PATH="$LIB_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
export LIBRARY_PATH="$LIB_DIR${LIBRARY_PATH:+:$LIBRARY_PATH}"
export PLASMITE_LIB_DIR="$LIB_DIR"
export PLASMITE_BIN="$LIB_DIR/plasmite"
export GOCACHE="$ROOT/.scratch/go-build"
mkdir -p "$ROOT/.scratch/go-build"

expected_version="$(
  sed -n 's/^pub const POOL_FORMAT_VERSION: u32 = \([0-9][0-9]*\);/\1/p' "$ROOT/src/core/format.rs"
)"
if [[ -z "$expected_version" ]]; then
  echo "failed to detect POOL_FORMAT_VERSION from src/core/format.rs"
  exit 1
fi

check_pool_version() {
  local pool_path="$1"
  local actual
  actual="$(od -An -j4 -N4 -tu4 "$pool_path" | tr -d '[:space:]')"
  if [[ "$actual" != "$expected_version" ]]; then
    echo "expected format version $expected_version, got $actual ($pool_path)"
    exit 1
  fi
}

echo "[smoke] cli -> pool"
"$PLASMITE_BIN" --dir "$WORKDIR" pool create cli-pool --size 1048576 >/dev/null
check_pool_version "$WORKDIR/cli-pool.plasmite"

echo "[smoke] node -> pool"
(
  cd "$ROOT"
  node -e "const { Client } = require('./bindings/node'); const c = new Client(process.argv[1]); const p = c.createPool('node-pool', 1048576n); p.close(); c.close();" "$WORKDIR"
)
check_pool_version "$WORKDIR/node-pool.plasmite"
"$PLASMITE_BIN" --dir "$WORKDIR" pool info node-pool --json >/dev/null

echo "[smoke] python -> pool"
PYTHONPATH="$ROOT/bindings/python" python3 - <<'PY' "$WORKDIR"
import sys
from plasmite import Client

workdir = sys.argv[1]
client = Client(workdir)
pool = client.create_pool("python-pool", 1048576)
pool.close()
client.close()
PY
check_pool_version "$WORKDIR/python-pool.plasmite"
"$PLASMITE_BIN" --dir "$WORKDIR" pool info python-pool --json >/dev/null

echo "[smoke] go -> pool"
cat > "$WORKDIR/go-smoke.json" <<'JSON'
{
  "conformance_version": 0,
  "name": "go-create-smoke",
  "workdir": "go-work",
  "steps": [
    {
      "op": "create_pool",
      "pool": "go-pool",
      "input": {
        "size_bytes": 1048576
      }
    }
  ]
}
JSON
(
  cd "$ROOT/bindings/go"
  CGO_LDFLAGS="-L$LIB_DIR" go run ./cmd/plasmite-conformance "$WORKDIR/go-smoke.json"
)
check_pool_version "$WORKDIR/go-work/go-pool.plasmite"
"$PLASMITE_BIN" --dir "$WORKDIR/go-work" pool info go-pool --json >/dev/null

echo "[smoke] complete"
