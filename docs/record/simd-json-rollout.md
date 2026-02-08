<!--
Purpose: Define portability assumptions for the simd-json-only parser rollout.
Exports: v0 support matrix, CI expectations, and mitigation playbook.
Role: Release record guiding support commitments and incident response.
Invariants: Parser rollout gates must align with the documented support matrix.
Invariants: CI expectations and mitigation steps stay synchronized with release policy.
Notes: Target baseline is intentionally conservative until broader coverage is validated.
-->

# simd-json rollout portability baseline

## v0 target assumptions

- Supported architecture/runtime baseline for v0:
  - Linux x86_64 (glibc)
  - macOS x86_64
  - macOS aarch64
- Best-effort (not release-blocking) targets:
  - Linux aarch64
  - Windows x86_64
- Unsupported for v0 unless explicitly promoted later:
  - 32-bit architectures
  - musl-only or embedded/no-std targets

## support matrix

| Platform | Arch | Status | Notes |
| --- | --- | --- | --- |
| Linux | x86_64 | Supported | Primary CI gate |
| macOS | x86_64 | Supported | Primary CI gate |
| macOS | aarch64 | Supported | Primary CI gate |
| Linux | aarch64 | Best effort | Run on scheduled/optional jobs |
| Windows | x86_64 | Best effort | Validate basic parser behavior |

## CI expectations

- Required CI for parser changes:
  - `cargo check --all-targets`
  - `cargo test`
  - parser-focused integration tests on Linux x86_64 and macOS runners.
- Recommended non-blocking CI expansion:
  - scheduled/ahead-of-release checks for Linux aarch64 and Windows x86_64.
- Known gaps for initial rollout:
  - no guaranteed SIMD parity validation on every non-primary architecture per PR.

## parse-failure categories in diagnostics

Runtime parse failures are categorized for operator diagnostics and incident triage.

- Stable category labels:
  - `syntax`
  - `utf8`
  - `numeric-range`
  - `depth-limit`
  - `unknown`
- Surface behavior:
  - CLI/ABI/runtime parse errors may include hints with `parse category: <label>`.
  - Hints also include a context identifier (for example ingest mode or remote endpoint seam).
- Scope:
  - Categories are diagnostics-facing and implementation-stable for rollout tracking.
  - They are not currently a versioned external schema contract.

## rollback and mitigation policy

- If architecture-specific parser failures appear on supported targets:
  1. Pause release and mark parser rollout as degraded.
  2. Reproduce with captured payload + target triple.
  3. Ship hotfix that preserves error-category compatibility.
  4. Backport fix to maintained branches before re-enabling rollout.
- If failures appear on best-effort targets only:
  1. Keep release open unless data-loss/corruption risk exists.
  2. Add explicit known-issue entry and workaround in release notes.
  3. Escalate target to supported only after CI signal is stable.

## performance baseline (simd-json-only)

- Artifact: `.scratch/bench/simd-json-only-baseline.json`
- Command:
  - `cargo build --release --example plasmite-bench`
  - `./target/release/examples/plasmite-bench --format json > .scratch/bench/simd-json-only-baseline.json`
- Environment notes:
  - `os=macos`, `arch=aarch64`, `cpus=8`
  - release profile, default benchmark parameter matrix from harness

Top metrics (best observed run per bench in this artifact):

- `append`: `74,812 msgs/sec` (`1,168.94 MB/sec`) at `payload=16,384B`, `pool=1MiB`, `writers=1`
- `multi_writer`: `21,668 msgs/sec` (`338.58 MB/sec`) at `payload=16,384B`, `pool=1MiB`, `writers=2`
- `get_scan`: `12,048,192 msgs/sec` (`188,253.01 MB/sec`) at `payload=16,384B`, `pool=64MiB`, `writers=1`

Caveats and non-goals:

- Single-host sample; values are baseline anchors, not SLO targets.
- Harness includes very short-duration scans where timer granularity can inflate
  apparent throughput (`get_scan` especially).
- `follow` bench is latency-oriented in this harness and can emit null
  throughput fields; do not compare it directly with append/scan throughput.
- Cross-architecture comparisons are out of scope for this artifact; compare
  only against later runs with matching hardware + benchmark parameters.

## Consult Me

- Confirm whether Linux aarch64 should be upgraded from best effort to fully
  supported in v0 once CI capacity is available.
