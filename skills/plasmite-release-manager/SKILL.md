---
name: "plasmite-release-manager"
description: "Carefully run Plasmite releases end-to-end with fail-closed pre-release QA, split build/publish workflow mechanics, provenance validation, and post-release delivery verification across crates.io, npm, PyPI, Homebrew, and release artifacts. Use when asked to prepare, dry-run, execute, audit, or recover a release (including publish-only reruns after credential fixes)."
---

# Plasmite Release Manager

Use this runbook for preparation, rehearsal, publication and recovery. Record the
release target (`vX.Y.Z`), mode (`dry-run` or `live`), agent and previous release tag.
Honor any requested review or publication hold before remote mutations.

## Prepare the exact candidate

1. Check `gh auth status` and access to GitHub Actions artifacts and registries.
   Keep a clean maintainer checkout of `../homebrew-tap` available.
2. Inspect the complete diff from the previous release. Finalize the versioned
   changelog with the actual release date and a fresh empty `[Unreleased]` heading.
   Review the complete release body: the publish workflow appends install guidance.
3. Run `bash scripts/bump_version.sh <version>`, then
   `bash scripts/check-version-alignment.sh`.
4. Initialize or reopen evidence:
   `bash skills/plasmite-release-manager/scripts/init_release_evidence.sh --release-target <vX.Y.Z> --mode <dry-run|live> --agent <model@host>`.
5. Run `just release-gate` and `just audit` (the latter refreshes the advisory DB).
   For hot-path/storage changes, also run
   `bash skills/plasmite-release-manager/scripts/compare_local_benchmarks.sh --base-tag <base_tag> --runs 3`.
   Failed required checks block progression. Do not duplicate the canonical gate's
   individual test commands or benchmark unchanged code.
6. Commit and push the reviewed candidate, including versions and changelog.
   Require a clean working tree and confirm the intended remote ref names this SHA.
   Set `release_sha="$(git rev-parse HEAD)"`, then run:
   `gh run list --workflow ci.yml --commit "$release_sha" --limit 5 --json headSha,status,conclusion,url`.
   Require `headSha == release_sha`, `status == completed`, `conclusion == success`.
   Changes after this point require a new candidate and new exact-SHA CI.

## Build, rehearse and publish

1. Tag the green candidate: `git tag <release_target> "$release_sha"`, then
   `git push origin <release_target>`. Never move an existing release tag.
   The tag starts `release.yml`; all matrix jobs must pass. Record its run ID/URL.
   Release builds use Rust 1.88.0 and the targets in `release/targets.json`.
2. Verify provenance before using artifacts:
   `bash skills/plasmite-release-manager/scripts/inspect_release_build_metadata.sh --run-id <build_run_id> --expect-tag <release_target>`.
   Require the metadata's source SHA to equal the reviewed candidate. If downloads
   are unavailable, use another authorized runtime; do not skip verification.
3. Rehearse for a dry run, release/packaging/target/publishing changes, or changed
   registry credentials:
   `gh workflow run release-publish.yml -f release_tag=<release_target> -f rehearsal=true`.
   Require success. Rehearsal checks a temporary expected formula; it does not
   require pushing the public Homebrew tap. **A dry run stops here.**
   An unchanged normal live release may proceed directly to step 4.
4. For an authorized live release, prepare and review the tap formula:
   `bash scripts/update_homebrew_formula.sh <release_target> ../homebrew-tap --build-run-id <build_run_id>`.
   Commit only `Formula/plasmite.rb` as `plasmite: update to <version>` and push.
   Immediately dispatch:
   `gh workflow run release-publish.yml -f release_tag=<release_target> -f rehearsal=false`.
   Record the publish run ID/URL and require every job to succeed.
5. The workflow verifies tag/build provenance and tap version/URLs/checksums before
   any registry publication. It uses npm trusted publishing and the
   `PYPI_API_TOKEN` / `CARGO_REGISTRY_TOKEN` secrets. It publishes crates.io, npm
   and PyPI, then attaches SDKs and checksums to the GitHub Release.
   The current order briefly points the tap at assets not yet public. If publishing
   stalls before assets exist, restore the prior formula with a normal commit;
   realign it before retrying. Do not leave users with broken download URLs.

## Recover without guessing

1. Stop progression and inspect the recorded run:
   `gh run view <run_id> --json url,jobs`, then `gh run view <run_id> --log-failed`.
   A generic HTTP 403 is an authorization error, not proof of publication.
2. Check the exact version on **each** registry. Before accepting an existing
   package, compare npm/PyPI artifact hashes with the selected build and establish
   Rust source provenance. Existing versions are immutable; do not overwrite them.
3. Correct the specific failure (credentials, tap alignment or delivery), then
   rerun the same publish target. After credential recovery, rehearse first.
   Prefer `release_tag`; explicit `build_run_id` is for a verified recovery run.
   The publish workflow handles already-published versions; do not manually create
   a GitHub Release merely because one registry says a version exists.
4. Record unresolved blockers using
   `bash skills/plasmite-release-manager/scripts/file_release_blocker_with_evidence.sh --release-target <release_target> --check "<gate>" --title "<title>" --summary "<summary>" --run-id <run_id> --agent <model@host>`.
   Partial publication is not a completed release.

On resume, reopen the evidence and use its **target, source SHA and build/publish
run IDs**, not the newest workflow run. Recheck the remote tag and exact-SHA CI.

## Verify public delivery

1. Inspect `gh release view <release_target>`: reviewed notes, all manifest SDK
   assets and `sha256sums.txt`. Verify downloaded SDK bytes against those checksums.
2. Check exact registry versions, not only latest:
   `cargo info plasmite@<version>`, `npm view plasmite@<version> version dist.integrity`,
   and `curl -fsS https://pypi.org/pypi/plasmite/<version>/json`.
3. Dispatch `gh workflow run post-release-smoke.yml -f release_version=<version>`.
   Require Linux npm/crates (also PyPI when the release declares Linux wheels),
   macOS PyPI, and macOS Homebrew jobs to pass.
   This checks published installs, not just build artifacts.
4. For local verification, the script accepts only `npm,pypi,crates`:
   `bash scripts/post_release_delivery_smoke.sh --version <version> --channels npm,pypi,crates`.
   Run PyPI on a wheel-supported host (currently CPython 3.11 macOS/Windows), or
   with the documented system SDK. Linux without that SDK should use `npm,crates`.
   Homebrew is separate: install the formula on macOS and require the exact version.
5. Archive results and exact-SHA CI evidence. A missing or failed required channel
   blocks completion. Clean generated builds/successful-test scratch with
   `cargo clean`, preserving source, release evidence and active Ergo state.
