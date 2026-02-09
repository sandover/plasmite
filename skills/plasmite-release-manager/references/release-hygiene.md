<!--
Purpose: Define mechanical release steps for Plasmite using GitHub tooling.
Key outputs: Tag/release artifacts and evidence of publish workflow completion.
Role: Canonical runbook after QA gates pass.
Invariants: Do not publish if any release blocker task is open.
Notes: Prefer `gh` commands for repeatability and auditability.
-->

# Release Hygiene (Mechanical Flow)

Run this only after all required QA gates pass and no blocker tasks remain.

## Preconditions

- Working tree clean for release intent.
- `gh auth status` is healthy.
- `ergo --json list --epic <release-blocker-epic-id>` has no non-done blocker tasks.
- Version alignment passes:
  - `bash scripts/check-version-alignment.sh`

## Runtime Access Requirements

- Use a runtime with network access and host-backed `gh` authentication.
- If `gh` reports connection/auth errors in sandboxed mode, re-run unsandboxed/escalated and confirm with:
  - `gh auth status`
  - `gh api user -q .login`
- Do not proceed with tag/release/publish steps unless these checks pass in the active runtime.

## Prepare Candidate

1. Confirm version fields are aligned.
   - `bash scripts/check-version-alignment.sh`
2. Run full required hygiene gates.
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test`
3. Build and smoke release artifacts.
   - `bash scripts/package_release_sdk.sh`
   - `bash scripts/cross_artifact_smoke.sh`
4. Verify release workflow configuration.
   - `gh workflow list`
   - ensure release workflow exists and references expected targets.

## Tag + Release (Live Mode)

1. Create annotated tag:
   - `git tag -a vX.Y.Z -m "Release vX.Y.Z"`
2. Push tag:
   - `git push origin vX.Y.Z`
3. Watch release workflow:
   - `gh run list --branch main --limit 20`
   - `gh run watch <run-id>`
4. Confirm GitHub release exists and artifacts are attached:
   - `gh release view vX.Y.Z`
   - `gh release verify-asset vX.Y.Z <artifact-name>` (if available in current gh version)

## Dry-Run Mode

Use dry-run when validating process only:
- do not push tags
- do not publish packages
- run workflow simulations up to local artifact production

## Failure Handling

If any step fails:
1. Stop the release sequence.
2. File blocker task via `scripts/file_release_blocker.sh`.
3. Attach exact failing command and output summary in blocker task.
