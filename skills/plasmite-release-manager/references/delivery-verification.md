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

## PyPI

- `python3 -m pip index versions plasmite`
- or `curl -sS https://pypi.org/pypi/plasmite/json | jq -r '.info.version'`
- verify current published version equals `X.Y.Z`

## Homebrew Tap (if updated for this release)

- confirm formula/tag update location and version fields:
  - `rg -n "version|sha256|url" homebrew/plasmite.rb`
- optional remote check:
  - verify tap update commit/tag is present for this release

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
