# Windows graduation gates (preview → official release)

## Goal
Define objective criteria for promoting Windows from preview artifacts to an official release channel.

## Scope decision
- **Phase 1 official scope:** `x86_64-pc-windows-msvc` only.
- **Explicit Phase 1 non-goal:** `aarch64-pc-windows-msvc` (Windows ARM64) is deferred to Phase 2.
- ARM64 is reconsidered only after Phase 1 gates are stable and CI capacity exists for meaningful ARM64 validation.

## Graduation gates

### Gate 1: reliability on `main`
- Requirement: `10` consecutive `main` runs where the `windows smoke` job executes and concludes `success`.
- Source of truth: `.github/workflows/ci.yml` (`WINDOWS_SMOKE_PROMOTE_AFTER_GREEN_MAIN_RUNS=10`).
- Counting rule: only runs where `windows smoke` actually ran count toward the streak.

### Gate 2: minimum runtime smoke coverage
- Required CLI coverage on Windows (`windows smoke` job):
  - `plasmite.exe --version`
  - local pool roundtrip (`pool create` + `poke` + `peek`) with payload assertion
- Required non-CLI client-path coverage:
  - keep at least one binding/remote path smoke active (`scripts/node_remote_only_smoke.sh` in CI) while Windows is promoted.
- Rationale: this keeps graduation tied to both core CLI behavior and at least one user-facing client path without requiring immediate native Windows binding packaging.

### Gate 3: release/provenance workflow decision
- **Decision:** do not keep `windows-preview.yml` as the long-term official path.
- Promotion requires moving Windows build/package into official release flow (`release.yml` + `release-publish.yml`) with parity on:
  - artifact naming/layout contract
  - SHA256 sidecar/checksum publication
  - release metadata/provenance checks already enforced for official artifacts

### Gate 4: rollback policy
- Promotion to blocking/official must keep the rollback controls in `docs/planning/windows-preview-rollback.md`.
- If post-promotion regression appears, demote Windows back to preview/non-blocking using the documented trigger:
  - `2` failures in latest `5` `main` runs, or
  - one deterministic, reproducible Windows regression.

## Current preview blockers (as of 2026-02-15)
- Reliability streak not yet met (`3/10` consecutive green `windows smoke` runs on `main`).
- Official release workflow does not yet build/publish Windows artifacts; preview still relies on manual `windows-preview.yml`.
- Native Windows Python/Node packaging remains intentionally deferred; only preview CLI artifact delivery is in place.
- Windows validation loop remains GitHub-hosted only (no local Windows builder), increasing triage latency.

## Graduation outcome
Windows exits preview only when all four gates above are satisfied and maintainer sign-off is recorded in the corresponding graduation PR.
