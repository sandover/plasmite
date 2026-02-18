---
name: "plasmite-release-manager"
description: "Carefully run Plasmite releases end-to-end with fail-closed pre-release QA, split build/publish workflow mechanics, provenance validation, and post-release delivery verification across crates.io, npm, PyPI, Homebrew, and release artifacts. Use when asked to prepare, dry-run, execute, audit, or recover a release (including publish-only reruns after credential fixes)."
---

# Plasmite Release Manager

## Inputs And Preconditions

Required inputs from maintainer:
- `release_target` (for example `v0.1.10`)
- `mode` (`dry-run` or `live`)
- `agent_id` (`model@host`)

Before running any release step:
1. Confirm `release_target`, `mode`, and `agent_id` explicitly.
2. Derive `base_tag` automatically as the highest semver tag lower than `release_target`:
   - `bash skills/plasmite-release-manager/scripts/init_release_evidence.sh --release-target <vX.Y.Z> --mode <dry-run|live> --agent <model@host>`
   - Read `Base tag:` from the generated evidence report and reuse that value for any base-tag-driven commands.
3. Verify runtime access:
   - `gh auth status`
   - network access for GitHub + registries
4. Verify version alignment:
   - `bash scripts/check-version-alignment.sh`
5. Open or initialize evidence report (idempotent):
   - `bash skills/plasmite-release-manager/scripts/init_release_evidence.sh --release-target <vX.Y.Z> --mode <dry-run|live> --agent <model@host>`
6. Verify required repository secrets are configured:
   - always: `NPM_TOKEN`, `PYPI_API_TOKEN`, `CARGO_REGISTRY_TOKEN`
   - live publish only: `HOMEBREW_TAP_TOKEN`

Release invariants (non-negotiable):
1. Publish only from a successful `release` build run with verified metadata.
2. Homebrew formula alignment must pass before registry publish.
3. Release remains fail-closed: all channels publish or none do.
4. Registry versions must align with `release_target`.
5. `HOMEBREW_TAP_TOKEN` must be configured for live publish runs.
6. Release workflows use pinned helper tooling:
   - `RELEASE_RUST_TOOLCHAIN=1.88.0`
   - `CARGO_BINSTALL_VERSION=1.17.5`

## Procedure

### 1) Pre-release QA

Always-run core gates:
- `cargo fmt --all`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `just bindings-test`
- `bash scripts/node_pack_smoke.sh`
- `bash scripts/node_remote_only_smoke.sh`
- `bash scripts/python_wheel_smoke.sh`
- `bash scripts/check_release_workflow_topology.sh`
- `bash skills/plasmite-release-manager/scripts/verify_licensing_notices.sh` (if artifacts exist)

Conditional gates (run when corresponding files changed since `base_tag`):
- Dependencies/security:
  - trigger: lockfiles/dependency manifests changed
  - command: `cargo audit --db .scratch/advisory-db --no-fetch --ignore yanked`
- Performance:
  - trigger: core hot path/storage code changed
  - command: `bash skills/plasmite-release-manager/scripts/compare_local_benchmarks.sh --base-tag <base_tag> --runs 3`
- Server/UI security:
  - trigger: server/auth/UI/spec surface changed
  - commands: `cargo test -q --test remote_integration` and focused source review

Gate policy:
- Any failed required gate blocks release.
- For ordinary test/tool failures: fix and rerun (no blocker task required).
- File ergo blockers only for incidents:
  - workflow failures requiring follow-up code changes
  - partial-publish/channel asymmetry
  - policy exceptions requiring maintainer decision

### 2) Short Resume Checkpoint

If interrupted, run these three commands before resuming:
```bash
git status --short --branch
gh run list --workflow release --limit 1 --json databaseId,conclusion
gh run list --workflow release-publish --limit 1 --json databaseId,conclusion
```

Escalate to incident workflow only when short checkpoint reveals anomalies
(partial publish, provenance mismatch, conflicting tags, failed rerun).

### 3) Build And Publish Mechanics

The `release-publish` workflow is manual-dispatch-only (`workflow_dispatch`). There is no automatic trigger from the `release` build workflow.

1. Ensure release source is pushed and tag exists/planned.
2. Run release build workflow (`release`):
   - push tag `vX.Y.Z` or dispatch `release.yml` with `tag`
   - require successful build run
3. Optionally prove build provenance explicitly before publish:
   - `bash skills/plasmite-release-manager/scripts/inspect_release_build_metadata.sh --run-id <build_run_id> --expect-tag <release_target>`
4. Rehearsal publish run (always before live publish):
   - `gh workflow run release-publish.yml -f release_tag=<release_target> -f rehearsal=true`
5. Live publish run:
   - `gh workflow run release-publish.yml -f release_tag=<release_target> -f rehearsal=false`
6. `release-publish.yml` resolves a successful build run for the tag, verifies release metadata, syncs Homebrew tap, then publishes.
7. Prefer `release_tag` dispatches for normal operation; use explicit `build_run_id` only for incident recovery.
8. For credential/transient failures, do publish-only rerun using the same `release_target`.
9. If needed for incident recovery, dispatch with explicit build run:
   - `gh workflow run release-publish.yml -f build_run_id=<build_run_id> -f rehearsal=false`

### Release Tooling Stability

- Prefer pinned binary installs for external helper tools in release workflows (for example `cargo-binstall`) instead of `cargo install` from source.
- Keep helper-tool versions explicit in workflow constants and bump intentionally.
- If source compilation is unavoidable, pin an explicit Rust toolchain for that step and validate the tool's minimum supported Rust version before bumping.

### 4) Failure Handling

If any release workflow fails:
1. Stop release progression.
2. If a matrix job fails while the overall run is still in progress, fetch job-level logs immediately (do not wait for run-level failed-log aggregation):
   - list failed jobs:
     - `gh run view <run-id> --json jobs --jq '.jobs[] | select(.status=="completed" and .conclusion=="failure") | {id:.databaseId,name}'`
   - fetch logs per failed job:
     - `gh api repos/sandover/plasmite/actions/jobs/<job-id>/logs`
   - extract fast triage signals:
     - `gh api repos/sandover/plasmite/actions/jobs/<job-id>/logs | rg -n "error:|Process completed with exit code|cannot find -lplasmite|linking with|unsupported platform"`
   - this is triage-only; release gating decisions remain unchanged.
3. Capture machine-readable failure evidence:
   - `gh run view <run-id> --json url,jobs --jq '{url,jobs:[.jobs[]|select(.conclusion=="failure")|{name,url:.url}]}'`
   - `gh run view <run-id> --log-failed`
   - If `sync-homebrew-tap` fails, check `HOMEBREW_TAP_TOKEN` and rerun `release-publish` for the same `release_target`.
4. If incident-class failure, file blocker:
   - `bash skills/plasmite-release-manager/scripts/file_release_blocker_with_evidence.sh --release-target <release_target> --check "<gate>" --title "<title>" --summary "<summary>" --run-id <run-id> --agent <model@host>`

## Post-Release Verification

Mandatory every release (fast checks):
1. GitHub release exists with expected assets:
   - `gh release view <release_target>`
2. crates.io version:
   - `cargo info plasmite`
3. npm version:
   - `npm view plasmite version`
   - optional install verification:
     - `npm pack plasmite@latest`
     - install tarball in temp dir and verify `node_modules/plasmite/native/linux-x64/index.node` exists
4. PyPI version:
   - `curl -sS https://pypi.org/pypi/plasmite/json | jq -r '.info.version'`
5. Homebrew formula alignment:
   - `gh api repos/sandover/homebrew-tap/contents/Formula/plasmite.rb -H "Accept: application/vnd.github.raw"`

Weekly scheduled checks (not release-blocking per release):
- clean-environment install sanity for Node/Python/Go bindings
- licensing/notices verification across local artifacts
- implemented by `.github/workflows/weekly-install-sanity.yml`

Block release immediately if:
- any channel resolves the wrong version
- release assets are missing/corrupt
- channel publish asymmetry is detected
