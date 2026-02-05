<!--
Purpose: Provide a concise, reproducible checklist for tagging and publishing releases.
Exports: N/A (documentation).
Role: Maintainer runbook; complements CI and the release workflow configuration.
Invariants: Local gates must match CI (fmt/clippy/test); steps should work from a clean checkout.
Notes: Keep example version/tag values in sync with the latest release.
-->

# Releasing Plasmite

This checklist keeps releases reproducible and ensures CI gates are green before tagging.

## Pre-flight

1. Sync and verify a clean workspace:
   - `git fetch --all --prune`
   - `git status -sb` (should be clean)

2. Run local gates:
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test --all --locked`

3. Review CI status on `main`:
   - Ensure the `ci` workflow is green on the latest `main` commit.

## Tag and release

1. Create the tag:
   - `git tag v0.0.1`
   - `git push origin v0.0.1`

2. Monitor the `release` workflow:
   - Verify artifacts are produced for all targets.
   - Verify `SHA256SUMS` is published.

## Notes

- CI already enforces fmt/clippy/test on PRs (see `.github/workflows/ci.yml`).
- Release artifacts are built and uploaded by `.github/workflows/release.yml`.
