# Delivery Verification

Use this after release workflows finish.

Inputs:
- `release_target` (for example `v0.1.1`)
- `version` without `v` (for example `0.1.1`)
- evidence report path (for example `.scratch/release/evidence-v0.1.1.md`)

## Build Provenance

Before verifying channels, confirm the publish run was sourced from a successful build run for the same tag:
- `bash skills/plasmite-release-manager/scripts/inspect_release_build_metadata.sh --run-id <build-run-id> --expect-tag <release_target>`

## GitHub Release Artifacts

- `gh release view vX.Y.Z`
- `gh release download vX.Y.Z --dir .scratch/release/downloaded`
- verify expected SDK layout in downloaded archives:
  - `bin/plasmite`, `bin/pls`, `include/plasmite.h`, `lib/pkgconfig/plasmite.pc`

## crates.io

- `cargo info plasmite`
- verify latest released version equals `X.Y.Z`

## npm

- `npm view plasmite version`
- `npm view plasmite dist-tags --json`
- verify `latest` (or intended dist-tag) resolves to `X.Y.Z`
- optional bundled-native check:
  - install latest tarball in a clean temp dir
  - verify `node_modules/plasmite/native/linux-x64/index.node` exists

## PyPI

- `curl -sS https://pypi.org/pypi/plasmite/json | jq -r '.info.version'`
- verify current published version equals `X.Y.Z`

## Homebrew Tap (required)

- confirm published tap formula resolves to this release version:
  - `gh api repos/sandover/homebrew-tap/contents/Formula/plasmite.rb -H "Accept: application/vnd.github.raw" | rg -n 'version|plasmite_[0-9.]+_(darwin_amd64|darwin_arm64|linux_amd64)|sha256'`
- verify formula checksums/urls match release artifacts:
  - `gh release download vX.Y.Z --pattern 'sha256sums.txt' --dir .scratch/release`
  - `bash scripts/verify_homebrew_formula_alignment.sh --version X.Y.Z --sha256sums .scratch/release/sha256sums.txt`

## Licensing and Notices

Run a deterministic artifact check:
- `bash skills/plasmite-release-manager/scripts/verify_licensing_notices.sh`
- treat `exit 2` as incomplete evidence (no artifacts found locally yet)
- treat `exit 1` as release-blocking notice/license drift

## Binding Install Sanity

Use clean envs where practical:
- Python:
  - `uv tool install plasmite==X.Y.Z`
  - `plasmite --version`
- Node:
  - `npm i -g plasmite@X.Y.Z`
  - `plasmite --version`
- Go:
  - `go get github.com/sandover/plasmite/bindings/go/plasmite@vX.Y.Z`

## Blocker Policy

File blocker task immediately if:
- channel not live after reasonable propagation window
- wrong version resolves from registry
- package installs but fails basic smoke (`--version` or minimal operation)
- release artifact missing required SDK contents
- release is partially published (one or more channels live while others failed)
- Homebrew formula lags or mismatches this release version/checksums

## Partial Publish Incident Handling

If any channel has published while release workflow conclusion is failure:
- treat this as a release-blocking incident, not a transient warning
- record exactly which channels are live vs missing
- capture failed workflow evidence:
  - `gh run view <run-id> --json url,jobs --jq '{url,jobs:[.jobs[]|{name,status,conclusion}]}'`
  - `gh run view <run-id> --log-failed`
- file/update a single `ergo` blocker summarizing channel asymmetry and recovery plan
  - preferred:
  - `bash skills/plasmite-release-manager/scripts/file_release_blocker_with_evidence.sh --release-target <release_target> --check "Delivery verification" --title "Resolve partial publish incident" --summary "Release channels are asymmetric after failed workflow." --run-id <run-id> --agent <model@host>`
- do not re-tag; continue with corrective commits and a follow-up patch release target
