#!/usr/bin/env bash
# Purpose: Produce a deterministic runtime benchmark artifact for README perf tables.
# Key exports: None; invoke as `scripts/bench_runtime_lanes.sh [output_json]`.
# Role: Build and run the release `plasmite-bench` example across canonical runtime lanes.
# Invariants: Writes one JSON artifact containing feed/follow/fetch and contention scenarios.
# Invariants: Uses repeatable pool/payload/durability inputs for stable docs refreshes.
# Invariants: Script is fail-fast (`set -euo pipefail`) and creates output directories as needed.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

OUTPUT_PATH="${1:-tmp/bench-runtime-lanes.json}"
POOL_SIZE="${POOL_SIZE:-64M}"
MESSAGES="${MESSAGES:-20000}"
PAYLOAD_SMALL="${PAYLOAD_SMALL:-256}"
PAYLOAD_BASE="${PAYLOAD_BASE:-1024}"
PAYLOAD_LARGE="${PAYLOAD_LARGE:-2048}"

mkdir -p "$(dirname -- "${OUTPUT_PATH}")"

cargo build --release --example plasmite-bench

target/release/examples/plasmite-bench \
  --pool-size "${POOL_SIZE}" \
  --payload-bytes "${PAYLOAD_SMALL}" \
  --payload-bytes "${PAYLOAD_BASE}" \
  --payload-bytes "${PAYLOAD_LARGE}" \
  --messages "${MESSAGES}" \
  --writers 1 \
  --writers 4 \
  --durability fast \
  --durability flush \
  --format json \
  > "${OUTPUT_PATH}"

printf 'Wrote benchmark artifact: %s\n' "${OUTPUT_PATH}"
