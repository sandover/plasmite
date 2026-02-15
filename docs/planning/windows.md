# Windows support plan

## Build strategy (Lite3 compiler path)

### Goal
Pick the lowest-risk compiler path for Windows Lite3 compilation.

### Decision: `clang-cl`

Set `CC_x86_64_pc_windows_msvc=clang-cl` for C compilation while keeping the MSVC target/linker path.

**Why not `msvc-shim`?** Probe runs showed `cl.exe` fails on GCC builtins (`__builtin_expect`) and parser fallout in `lite3.h` / `lite3_context_api.h`. The shim path remains high-risk.

**`clang-cl` outcome:** Successful end-to-end `cargo build --release --bins` on `windows-latest` plus smoke (`plasmite.exe --version`). The default path in `build.rs` works without explicit `CC` override.

**Fallback:** Revert to `msvc-shim` only if `clang-cl` becomes unavailable or regresses.

### Probe evidence
- Comparative run: [22000655822](https://github.com/sandover/plasmite/actions/runs/22000655822)
- Confirmation run: [22001090359](https://github.com/sandover/plasmite/actions/runs/22001090359)
- Default-path validation: [22001410962](https://github.com/sandover/plasmite/actions/runs/22001410962)

---

## Graduation gates (preview → official release)

### Scope
- **Phase 1:** `x86_64-pc-windows-msvc` only.
- **Phase 2 (deferred):** `aarch64-pc-windows-msvc` — reconsidered only after Phase 1 gates are stable.

### Gate 1: reliability on `main`
`10` consecutive `main` runs where `windows smoke` concludes `success`.
Source of truth: `.github/workflows/ci.yml` (`WINDOWS_SMOKE_PROMOTE_AFTER_GREEN_MAIN_RUNS=10`).

### Gate 2: minimum runtime smoke coverage
- CLI: `--version` + local pool roundtrip (`pool create` + `poke` + `peek`) with payload assertion.
- Non-CLI: at least one binding/remote path smoke active (`scripts/node_remote_only_smoke.sh`).

### Gate 3: release/provenance workflow
Move Windows build/package into official release flow (`release.yml` + `release-publish.yml`) with parity on artifact naming, SHA256 sidecar, and provenance checks.

### Gate 4: rollback policy

**Preview mode:** non-blocking (`continue-on-error: true`).

**Promotion criteria:** `10` consecutive green `main` runs, no unresolved incidents, maintainer sign-off by `@brandonharvey`.

**Rollback trigger:** `2+` failures in latest `5` `main` runs, or one confirmed deterministic regression.

**Rollback action:**
1. Restore non-blocking mode.
2. Temporarily disable PR path filter trigger if failures are noisy/non-actionable.
3. Open/refresh tracking issue with run links, failure signature, and owner.
4. Re-enable after trigger condition clears and owner signs off.

**Owner:** `@brandonharvey` (backup: current release owner). Acknowledge within one business day.

### Current preview blockers (as of 2026-02-15)
- Reliability streak: `3/10` consecutive green runs.
- Official release workflow does not yet build/publish Windows artifacts.
- Native Windows Python/Node packaging deferred.
- Validation loop is GitHub-hosted only (no local Windows builder).

---

## Bindings scope (Python + Node)

### Decision summary
- **Python (`win_amd64`):** preview-only later (not yet official).
- **Node (`win32-x64` native):** defer; keep remote-only usage path.
- Re-evaluate after Windows smoke/write-path reliability reaches promotion criteria.

### Python constraints
- Loader (`__init__.py`) searches `.dylib`/`.so` only; needs `.dll` + `.exe` handling.
- Release matrix covers Linux/macOS only; no Windows wheel job.
- Even with packaging fixes, local write confidence is gated by unresolved Windows runtime behavior.
- **Path:** add as preview-only after runtime stabilization + loader/CLI wrapper support.

### Node constraints
- Platform maps and `package.json` files exclude Windows.
- Native staging expects `.so/.dylib` and `plasmite` (no `.dll`/`.exe`).
- **Path:** defer native packaging; remote-only JS API via `RemoteClient` remains usable.

### CI cost estimate
Windows packaging requires: build job(s), install/runtime smoke, release workflow integration, and triage loop for flaky regressions. Meaningful increase in CI time and failure surface.
