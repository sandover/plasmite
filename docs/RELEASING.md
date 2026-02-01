# Releasing Plasmite

Short checklist for cutting a release and publishing GitHub artifacts.

## 1) Prep

- Update version in `Cargo.toml` (and anywhere else it is surfaced).
- Update `CHANGELOG.md` with the release notes.
- Review docs for accuracy (`README.md`, `docs/TESTING.md`, `spec/v0/SPEC.md`).

## 2) Validate locally

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all --locked
cargo install cargo-audit --locked
mkdir -p .scratch
if [ -d .scratch/advisory-db/.git ]; then
  git -C .scratch/advisory-db pull --ff-only
else
  git clone https://github.com/RustSec/advisory-db.git .scratch/advisory-db
fi
cargo audit --db .scratch/advisory-db --no-fetch
```

## 3) Tag and push

- Create an annotated tag `vX.Y.Z` and push it:

```bash
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z
```

Pushing a `v*` tag triggers the GitHub Actions release workflow.

## 4) Verify release artifacts

- Check the GitHub Release for per-target tarballs.
- Verify the `SHA256SUMS` file is present and matches the artifacts:

```bash
shasum -a 256 plasmite-*.tar.gz
cat SHA256SUMS
```

## 5) Post-release

- If you maintain a Homebrew tap, update it from the new release artifacts.
