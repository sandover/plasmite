# Release Hygiene (Mechanical Flow)

Run this only after all required QA gates pass and no blocker tasks remain.

## Preconditions

- Working tree clean for release intent.
- Release source is fully pushed (no `git status --short --branch` `ahead N` state).
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
- Homebrew tap path is known and writable (default sibling checkout):
  - `../homebrew-tap`
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
   - `gh run list --workflow release-publish --limit 5`
   - for any candidate build rerun source:
     - `bash skills/plasmite-release-manager/scripts/inspect_release_build_metadata.sh --run-id <build-run-id> --expect-tag <release_target>`
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
   - `git status --short --branch` (must not show local commits ahead of origin)
2. Initialize or reopen release evidence report.
   - `bash skills/plasmite-release-manager/scripts/init_release_evidence.sh --release-target <vX.Y.Z> --base-tag <vX.Y.Z> --mode <dry-run|live> --agent <model@host>`
3. Run full required hygiene gates.
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test`
4. Build and smoke release-related artifacts locally.
   - `just bindings-test`
   - `bash scripts/cross_artifact_smoke.sh`
5. Verify release workflow configuration.
   - `gh workflow list`
   - ensure both workflows exist: `release` (build artifacts) and `release-publish` (registry publish + GitHub release).
   - verify `release-publish` requires successful build-run artifact provenance before publish/release jobs execute.
   - verify `release-publish` requires successful Homebrew tap alignment before publish jobs execute.
   - block release if publish/release can run without downloaded `release-metadata` from a successful `release` run.

## Tag + Release (Live Mode)

1. Create annotated tag:
   - `git tag -a vX.Y.Z -m "Release vX.Y.Z"`
2. Push tag:
   - `git push origin vX.Y.Z`
3. Track build workflow first:
   - `build_run_id=$(gh run list --workflow release --limit 1 --json databaseId,event,status,conclusion --jq '.[] | select(.event=="push") | .databaseId' | head -n1)`
   - `gh run view "$build_run_id" --json status,conclusion,jobs --jq '{status,conclusion,jobs:[.jobs[]|{name,status,conclusion}]}'`
   - require build run `conclusion=success` before any publish stage
4. Align Homebrew tap before final publish:
   - `bash scripts/update_homebrew_formula.sh <release_target> ../homebrew-tap --build-run-id <build-run-id>`
   - `cd ../homebrew-tap && git add Formula/plasmite.rb && git commit -m "plasmite: update to <X.Y.Z>" && git push`
5. Rehearse publish workflow (recommended after workflow changes):
   - `gh workflow run release-publish.yml -f build_run_id=<build-run-id> -f rehearsal=true`
   - `rehearsal_run_id=$(gh run list --workflow release-publish --limit 1 --json databaseId,event --jq '.[0].databaseId')`
   - `gh run view "$rehearsal_run_id" --json status,conclusion,jobs --jq '{status,conclusion,jobs:[.jobs[]|{name,status,conclusion}]}'`
   - require rehearsal `conclusion=success` before live publish
6. Dispatch live publish workflow:
   - `gh workflow run release-publish.yml -f build_run_id=<build-run-id> -f rehearsal=false -f allow_partial_release=false`
   - `publish_run_id=$(gh run list --workflow release-publish --limit 1 --json databaseId,event,status,conclusion --jq '.[0].databaseId')`
   - `gh run view "$publish_run_id" --json status,conclusion,jobs --jq '{status,conclusion,jobs:[.jobs[]|{name,status,conclusion}]}'`
   - require publish run `conclusion=success` before delivery verification
   - write build/rehearsal/publish run IDs + status into evidence report checkpoint
7. Publish-only rerun after fixing credentials (no rebuild):
   - validate candidate run provenance:
   - `bash skills/plasmite-release-manager/scripts/inspect_release_build_metadata.sh --run-id <build-run-id> --expect-tag <release_target>`
   - `gh workflow run release-publish.yml -f build_run_id=<build-run-id> -f rehearsal=false -f allow_partial_release=false`
   - for intentional channel bypass, set channel flag(s) and `allow_partial_release=true` in the same dispatch.
8. Confirm GitHub release exists and artifacts are attached:
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

## High-Signal Failure Patterns

- symptom: `release-publish` fails with run/workflow mismatch for `build_run_id`
  - likely cause: provided run came from a workflow other than `release`
  - immediate fix: choose a successful `release` run and validate with:
    - `bash skills/plasmite-release-manager/scripts/inspect_release_build_metadata.sh --run-id <id> --expect-tag <release_target>`
- symptom: `release-publish` preflight fails before any publish jobs
  - likely cause: missing/invalid registry credentials or policy mismatch (npm OTP/2FA)
  - immediate fix: follow preflight hint text, update secrets/policy, then run publish-only rerun
- symptom: `verify-homebrew-tap` fails
  - likely cause: tap formula version/sha/url is stale for the target tag
  - immediate fix: run `scripts/update_homebrew_formula.sh <release_target> ../homebrew-tap --build-run-id <build-run-id>`, push tap commit, then rerun `release-publish`
- symptom: manual release build succeeds but version/tag artifacts are unexpected
  - likely cause: dispatch target/tag mismatch
  - immediate fix: confirm release workflow run metadata and artifact tag/version alignment before publish rerun
