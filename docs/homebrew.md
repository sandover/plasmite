# Homebrew tap setup

This repo ships release artifacts. Use a separate tap repo (recommended name: `homebrew-tap`) to publish the formula.
The formula installs both `plasmite` and the short alias `pls`.

## 1) Create the tap repo

- Create `github.com/YOUR_GITHUB/homebrew-tap`
- Add a `Formula/` folder

## 2) Add the formula

Copy `homebrew/plasmite.rb` into the tap repo at `Formula/plasmite.rb`.
Update `url` and `sha256` to match the release assets for each target.

## 3) Install from the tap

```bash
brew tap YOUR_GITHUB/tap
brew install plasmite
```

## 4) Update on release

Each `v*` tag triggers the release workflow and uploads assets:

- `plasmite-x86_64-apple-darwin.tar.gz`
- `plasmite-aarch64-apple-darwin.tar.gz`
- `plasmite-x86_64-unknown-linux-gnu.tar.gz`

Update the formula URLs and SHA256 values to match the new release assets.
