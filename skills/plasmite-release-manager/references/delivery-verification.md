# Delivery Verification

Use this after release workflows finish.

Inputs:
- `release_target` (for example `v0.1.1`)
- `version` without `v` (for example `0.1.1`)

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

