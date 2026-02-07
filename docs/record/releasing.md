<!--
Purpose: Provide a concise, reproducible checklist for tagging and publishing releases.
Exports: N/A (documentation).
Role: Maintainer runbook; complements CI and the release workflow configuration.
Invariants: Local gates must match CI (fmt/clippy/test); steps should work from a clean checkout.
Notes: Keep example version/tag values in sync with the latest release.
-->

# Releasing Plasmite

This checklist keeps releases reproducible and ensures CI gates are green before tagging.

## Versioning

- **Semver**: 0.1.0 is the first published release; pre-1.0 signals "real but evolving"
- **Lock-step**: CLI and all bindings (Go, Python, Node) release together with the same version
- **Tags**: `v0.1.0` format triggers the release workflow
- **CHANGELOG**: Follow [keep-a-changelog](https://keepachangelog.com) format

## Pre-flight

1. **Update version numbers** (all must match):
   - `Cargo.toml`: `version = "0.1.0"`
   - `bindings/node/package.json`: `"version": "0.1.0"`
   - `bindings/python/pyproject.toml`: `version = "0.1.0"`
   - Go uses git tags, no file change needed

2. **Update CHANGELOG.md**:
   - Move items from `[Unreleased]` to new `[0.1.0] - YYYY-MM-DD` section
   - Organize into Added/Changed/Fixed/Removed sections

3. **Sync and verify a clean workspace**:
   - `git fetch --all --prune`
   - `git status -sb` (should be clean on main)

4. **Run local gates**:
   - `just ci` (or `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test`)

5. **Review CI status on `main`**:
   - Ensure the `ci` workflow is green on the latest commit

## Tag and release

1. **Commit version bump**:
   ```bash
   git add Cargo.toml bindings/*/package.json bindings/*/pyproject.toml CHANGELOG.md
   git commit -m "Release 0.1.0"
   git push origin main
   ```

2. **Create and push tag**:
   ```bash
   git tag v0.1.0
   git push origin v0.1.0
   ```

3. **Monitor the release workflow**:
   - Watch https://github.com/sandover/plasmite/actions
   - Verify artifacts for the 3 supported release platforms (darwin arm64/amd64, linux amd64)
   - Verify `sha256sums.txt` is published
   - Linux arm64 (`aarch64-unknown-linux-gnu`) is best-effort from source builds and is not a release gate

4. **Update Homebrew tap**:
   ```bash
   ./scripts/update_homebrew_formula.sh v0.1.0 ../homebrew-tap
   cd ../homebrew-tap
   git add Formula/plasmite.rb
   git commit -m "plasmite: update to 0.1.0"
   git push
   ```

5. **Publish to registries**:
   - crates.io: Published by CI only when `CARGO_REGISTRY_TOKEN` is set; otherwise publish manually (`cargo publish`)
   - npm (manual):
     ```bash
     cd bindings/node
     npm publish
     ```
   - PyPI (manual):
     ```bash
     cd bindings/python
     python -m build
     python -m twine upload dist/*
     ```
     Ensure you have `build` and `twine` installed: `pip install build twine`

## One-time setup

Before the first release, configure GitHub secrets:

1. **CARGO_REGISTRY_TOKEN**: Create a token at https://crates.io/me/tokens, add it to GitHub repo secrets
2. **NPM_TOKEN**: (For npm publish workflow - see npm task)
3. **PYPI_TOKEN**: (For PyPI publish workflow - see PyPI task)

## Post-release

1. **Verify install paths**:
   - `cargo install plasmite` (after crates.io publish succeeds)
   - `brew install sandover/tap/plasmite` (after tap is pushed)
   - `npm install plasmite-node` (after npm publish)
   - `pip install plasmite` (after PyPI publish)

2. **Update README** if install instructions changed

## Notes

- CI already enforces fmt/clippy/test on PRs (see `.github/workflows/ci.yml`)
- Release artifacts are built and uploaded by `.github/workflows/release.yml`
- Homebrew formula is at `sandover/homebrew-tap/Formula/plasmite.rb`
