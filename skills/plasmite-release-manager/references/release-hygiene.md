# Release Hygiene (Mechanical Flow)

Run this only after all required QA gates pass and no blocker tasks remain.

## Preconditions

- Working tree clean for release intent.
- `gh auth status` is healthy.
- `ergo --json list --epic <release-blocker-epic-id>` has no non-done blocker tasks.
- Required publish credentials are present in GitHub repo secrets:
  - `CARGO_REGISTRY_TOKEN`
  - `PYPI_API_TOKEN`
  - `NPM_TOKEN`
- Explicit release context is confirmed with maintainer:
  - `release_target` (`vX.Y.Z`)
  - `base_tag` (existing prior tag)
  - `mode` (`dry-run` or `live`)
- Registry ownership/bootstrap checks are complete:
  - PyPI account exists and token can publish package name `plasmite` (first upload creates the project if available).
  - npm account/token can publish package name `plasmite`.
- Version alignment passes:
  - `bash scripts/check-version-alignment.sh`
- Evidence report exists and is current:
  - `bash skills/plasmite-release-manager/scripts/init_release_evidence.sh --release-target <vX.Y.Z> --base-tag <vX.Y.Z> --mode <dry-run|live> --agent <model@host>`

## Resume Checklist (Required After Interruption)

Run all of these before resuming any release mechanics:
1. Verify local state:
   - `git status --short --branch`
   - `git tag --list "v*.*.*" --sort=-version:refname | head -n 5`
2. Verify remote tag/workflow state:
   - `git ls-remote --tags origin | rg "refs/tags/<release_target>$"`
   - `gh run list --workflow release --limit 5`
3. Verify blocker state:
   - `ergo --json list --epics | jq -r '.[] | select(.title=="Release blockers: <release_target>") | .id'`
   - `ergo --json list --epic <release-blocker-epic-id>`
4. Update evidence report checkpoint fields before continuing.

## Runtime Access Requirements

- Use a runtime with network access and host-backed `gh` authentication.
- If `gh` reports connection/auth errors in sandboxed mode, re-run unsandboxed/escalated and confirm with:
  - `gh auth status`
  - `gh api user -q .login`
- Do not proceed with tag/release/publish steps unless these checks pass in the active runtime.

## Prepare Candidate

1. Confirm version fields are aligned.
   - `bash scripts/check-version-alignment.sh`
2. Initialize or reopen release evidence report.
   - `bash skills/plasmite-release-manager/scripts/init_release_evidence.sh --release-target <vX.Y.Z> --base-tag <vX.Y.Z> --mode <dry-run|live> --agent <model@host>`
3. Run full required hygiene gates.
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test`
4. Build and smoke release artifacts.
   - `bash scripts/package_release_sdk.sh`
   - `bash scripts/cross_artifact_smoke.sh`
5. Verify release workflow configuration.
   - `gh workflow list`
   - ensure release workflow exists and references expected targets.
   - verify publish jobs are gated by artifact jobs (`build-native`, `build-node-dist`, `build-python-dist`) via `needs` in `.github/workflows/release.yml`
   - block release if any publish job can run before required artifact jobs succeed

## Tag + Release (Live Mode)

1. Create annotated tag:
   - `git tag -a vX.Y.Z -m "Release vX.Y.Z"`
2. Push tag:
   - `git push origin vX.Y.Z`
3. Track release workflow with concise polling (preferred over verbose streaming):
   - `run_id=$(gh run list --workflow release --branch main --limit 1 --json databaseId,event,headBranch --jq '.[] | select(.event=="push" and .headBranch=="main") | .databaseId' | head -n1)`
   - `gh run view "$run_id" --json status,conclusion,jobs --jq '{status,conclusion,jobs:[.jobs[]|{name,status,conclusion}]}'`
   - repeat poll until `status=completed`
   - require `conclusion=success` before proceeding to delivery verification
   - write run ID and current status into the evidence report checkpoint
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
2. Capture machine-readable failure summary:
   - `gh run view <run-id> --json url,jobs --jq '{url,jobs:[.jobs[]|select(.conclusion=="failure")|{name,url:.url}]}'`
   - `gh run view <run-id> --log-failed`
3. File blocker task(s) via `scripts/file_release_blocker.sh` (at least one per distinct failure class).
   - preferred for workflow failures:
   - `bash skills/plasmite-release-manager/scripts/file_release_blocker_with_evidence.sh --release-target <release_target> --check "<gate>" --title "<title>" --summary "<summary>" --run-id <run-id> --failing-command "<command>" --agent <model@host>`
4. Attach exact failing command/log lines and run URL in blocker summary.
