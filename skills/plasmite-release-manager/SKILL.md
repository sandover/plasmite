---
name: "plasmite-release-manager"
description: "Carefully run Plasmite releases end-to-end with fail-closed pre-release QA, split build/publish workflow mechanics, provenance validation, and post-release delivery verification across crates.io, npm, PyPI, Homebrew, and release artifacts. Use when asked to prepare, dry-run, execute, audit, or recover a release (including publish-only reruns after credential fixes)."
---

# Plasmite Release Manager

Authoritative runbook for Plasmite releases. Keep execution fail-closed and aligned with workflow behavior.

## Required Inputs

- `release_target` (for example `v0.4.0`)
- `mode` (`dry-run` or `live`)
- `agent_id` (`model@host`)

## Release Invariants (Non-Negotiable)

1. Publish only from a successful `release` build run with matching release metadata.
2. `release-publish` must gate registry publish on Homebrew tap sync/alignment.
3. Release remains fail-closed across channels (no partial success treated as done).
4. Registry/package versions must equal `release_target`.
5. Secrets must exist:
   - always: `NPM_TOKEN`, `PYPI_API_TOKEN`, `CARGO_REGISTRY_TOKEN`
   - live publish: `HOMEBREW_TAP_TOKEN`
6. Tooling pins in workflows are policy:
   - `RELEASE_RUST_TOOLCHAIN=1.88.0`
   - `CARGO_BINSTALL_VERSION=1.17.5`

## Pre-Release Setup

1. Confirm runtime/auth:
   - `gh auth status`
2. Confirm version alignment:
   - `bash scripts/check-version-alignment.sh`
3. Initialize/reopen release evidence (derives `base_tag`):
   - `bash skills/plasmite-release-manager/scripts/init_release_evidence.sh --release-target <vX.Y.Z> --mode <dry-run|live> --agent <model@host>`

## Required QA Gates

Run before any publish dispatch:

- `cargo fmt --all`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `just bindings-test`
- `bash scripts/node_pack_smoke.sh`
- `bash scripts/node_remote_only_smoke.sh`
- `bash scripts/python_wheel_smoke.sh`
Run conditional gates when relevant:

- dependency/security changes: `cargo audit --db .scratch/advisory-db --no-fetch --ignore yanked`
- hot-path/storage changes: `bash skills/plasmite-release-manager/scripts/compare_local_benchmarks.sh --base-tag <base_tag> --runs 3`
- server/UI/auth changes: `cargo test -q --test remote_integration`

Any failed required gate blocks release progression.

## Build And Publish Procedure

1. Ensure release source is pushed and tag exists/planned.
2. Run `release.yml` (tag push or manual dispatch with `tag`).
3. Rehearse publish (required before live):
   - `gh workflow run release-publish.yml -f release_tag=<release_target> -f rehearsal=true`
4. Run live publish:
   - `gh workflow run release-publish.yml -f release_tag=<release_target> -f rehearsal=false`

Dispatch policy:

- Prefer `release_tag` dispatch for normal operation.
- Use explicit `build_run_id` only for incident recovery reruns.
- Optional explicit provenance proof:
  - `bash skills/plasmite-release-manager/scripts/inspect_release_build_metadata.sh --run-id <build_run_id> --expect-tag <release_target>`

## Failure Handling

On any release workflow failure:

1. Stop progression.
2. Collect evidence:
   - `gh run view <run-id> --json url,jobs --jq '{url,jobs:[.jobs[]|select(.conclusion=="failure")|{name,url:.url}]}'`
   - `gh run view <run-id> --log-failed`
3. If `sync-homebrew-tap` fails, verify `HOMEBREW_TAP_TOKEN`, then rerun for the same target.
4. If incident-class failure, file a blocker:
   - `bash skills/plasmite-release-manager/scripts/file_release_blocker_with_evidence.sh --release-target <release_target> --check "<gate>" --title "<title>" --summary "<summary>" --run-id <run-id> --agent <model@host>`

## Resume Checkpoint

If interrupted:

```bash
git status --short --branch
gh run list --workflow release --limit 1 --json databaseId,conclusion
gh run list --workflow release-publish --limit 1 --json databaseId,conclusion
```

## Post-Release Verification

Mandatory checks:

1. GitHub release + assets:
   - `gh release view <release_target>`
2. crates.io:
   - `cargo info plasmite`
3. npm:
   - `npm view plasmite version`
4. PyPI:
   - `curl -sS https://pypi.org/pypi/plasmite/json | jq -r '.info.version'`
5. Homebrew formula:
   - `gh api repos/sandover/homebrew-tap/contents/Formula/plasmite.rb -H "Accept: application/vnd.github.raw"`
