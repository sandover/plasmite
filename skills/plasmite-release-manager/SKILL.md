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
5. Secrets must exist: `NPM_TOKEN`, `PYPI_API_TOKEN`, `CARGO_REGISTRY_TOKEN`.
6. Homebrew tap is updated locally (not via CI secret). The maintainer pushes the formula from their `../homebrew-tap` checkout; the `sync-homebrew-tap` CI job verifies alignment.
7. Tooling pins in workflows are policy:
   - `RELEASE_RUST_TOOLCHAIN=1.88.0`
   - `CARGO_BINSTALL_VERSION=1.17.5`

## Pre-Release Setup

1. Confirm runtime/auth:
   - `gh auth status`
2. Confirm version alignment:
   - `bash scripts/check-version-alignment.sh`
3. Finalize the changelog:
   - Rename `## [Unreleased]` to `## [<version>] - <YYYY-MM-DD>` in `CHANGELOG.md`.
   - Review that the section accurately covers all notable changes since the previous release (check `git log <base_tag>..HEAD --oneline`).
   - Add a fresh empty `## [Unreleased]` heading above the new section.
   - The changelog must be committed and pushed before tagging. The `release` job extracts it for GitHub Release notes.
4. Bump version:
   - `bash scripts/bump_version.sh <version>`
5. Initialize/reopen release evidence (derives `base_tag`):
   - `bash skills/plasmite-release-manager/scripts/init_release_evidence.sh --release-target <vX.Y.Z> --mode <dry-run|live> --agent <model@host>`

## Required QA Gates

Run before any publish dispatch:

- Changelog: verify `CHANGELOG.md` has a `## [<version>] - <date>` section (not `[Unreleased]`) and the version matches the release target.
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
2. Run `release.yml` (tag push or manual dispatch with `tag`). Wait for all matrix jobs to succeed.
3. Update the Homebrew formula locally and push:
   - `bash scripts/update_homebrew_formula.sh <release_target> ../homebrew-tap --build-run-id <build_run_id>`
   - `cd ../homebrew-tap && git add Formula/plasmite.rb && git commit -m "plasmite: update to <version>" && git push`
4. Rehearse publish (recommended before first live dispatch):
   - `gh workflow run release-publish.yml -f release_tag=<release_target> -f rehearsal=true`
5. Run live publish:
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
3. If `sync-homebrew-tap` fails: update and push the formula from the local `../homebrew-tap` checkout (see Build And Publish Procedure step 3), then rerun.
4. If registries fail with "already published" (npm 403, crates.io "already exists"): the packages are live. Create the GitHub Release manually with `gh release create` using artifacts downloaded from the build run (`gh run download <build_run_id>`).
5. If incident-class failure, file a blocker:
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
