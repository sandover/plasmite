<!--
Purpose: Document how to package Plasmite via a Homebrew tap.
Exports: N/A (documentation).
Role: Maintainer/contributor guide for distribution.
Invariants: Steps must match the repo’s `homebrew/` formula and release artifact naming.
-->

# Homebrew tap setup

This repo ships release artifacts. Use a separate tap repo (recommended name: `homebrew-tap`) to publish the formula.
The formula installs the full SDK layout:

- `bin/plasmite` and `bin/pls`
- `lib/libplasmite.(dylib|so)` (+ `libplasmite.a` when shipped)
- `include/plasmite.h`
- `lib/pkgconfig/plasmite.pc`

## 1) Create the tap repo

- Create `github.com/YOUR_GITHUB/homebrew-tap`
- Add a `Formula/` folder

## 2) Add the formula

Copy `homebrew/plasmite.rb` into the tap repo at `Formula/plasmite.rb`.
Update `url` and `sha256` to match the release assets for each target.  
The asset tarball root must contain `bin/`, `lib/`, `include/`, and `lib/pkgconfig/`.

## 3) Install from the tap

```bash
brew tap YOUR_GITHUB/tap
brew install plasmite
pkg-config --modversion plasmite
```

## 4) Update on release

Each `v*` tag triggers the release workflow and uploads assets:

- `plasmite_<version>_darwin_amd64.tar.gz`
- `plasmite_<version>_darwin_arm64.tar.gz`
- `plasmite_<version>_linux_amd64.tar.gz`

Update the formula URLs and SHA256 values to match the new release assets.
