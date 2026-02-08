#!/usr/bin/env bash
# Purpose: Enforce runtime parser-boundary invariants for migrated callsites.
# Exports: Exit status only (0 pass, non-zero fail) for CI and local checks.
# Role: Prevent reintroduction of direct serde_json parse calls in runtime seams.
# Invariants: Runtime modules must use src/json/parse.rs helpers.
# Invariants: Benchmark/test-only parser usage is explicitly out of scope here.
# Notes: This guard intentionally scans only runtime files, not benches/tests/fixtures.

set -euo pipefail

runtime_files=(
  src/main.rs
  src/abi.rs
  src/api/message.rs
  src/api/remote.rs
  src/ingest.rs
)

pattern='serde_json::from_str|serde_json::from_slice'

if rg -n "${pattern}" "${runtime_files[@]}"; then
  cat <<'MSG' >&2
error: parser-boundary invariant violated.
direct serde_json parse calls are forbidden in runtime modules.
use crate::json::parse::from_str(...) and keep error mapping at the callsite.
MSG
  exit 1
fi

echo "ok: parser-boundary invariant holds for runtime modules"
