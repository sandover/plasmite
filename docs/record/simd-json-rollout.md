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

## Consult Me

- Confirm whether Linux aarch64 should be upgraded from best effort to fully
  supported in v0 once CI capacity is available.
