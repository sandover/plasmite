# Building Plasmite

## What gets built

- Rust crate: `plasmite` (CLI, library, tests, bindings support)
- Native C dependency: vendored Lite3 sources under `vendor/lite3/`
- C shim: `c/lite3_shim.c` exports the narrow ABI used by Rust FFI

`Cargo.toml` declares `build = "build.rs"`, so Cargo always runs the build script when needed.

## Native build model (Lite3 vendoring)

`build.rs` does three things:

1. Declares `cargo:rerun-if-changed` for shim and vendored Lite3 files.
2. Compiles vendored Lite3 C units plus `c/lite3_shim.c` into one static archive (`liblite3.a`) via `cc`.
3. Leaves native-link metadata to Cargo/rustc default integration from `cc`.

Key inputs:

- `vendor/lite3/src/lite3.c`
- `vendor/lite3/src/json_dec.c`
- `vendor/lite3/src/json_enc.c`
- `vendor/lite3/src/ctx_api.c`
- `vendor/lite3/src/debug.c`
- `vendor/lite3/lib/yyjson/yyjson.c`
- `vendor/lite3/lib/nibble_base64/base64.c`
- `c/lite3_shim.c`

If these vendored files are missing or empty, link failures will surface as unresolved `lite3_*` symbols.

## Local validation gates

Run the same core gates used for CI hygiene:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

For full CI-parity checks in this repo, run:

```bash
just ci
```

Packaging smoke (npm pack + wheel install) is covered in CI pull requests by
the `dist-smoke` job in `.github/workflows/ci.yml`.

## Python tooling policy

Use `uv` for Python environment and package operations in this project.

- Use `uv venv`, `uv pip`, and `uv tool` for local and CI automation.
- Do not add direct `pip`-based commands to docs or release runbooks.

## Release artifact matrix

`.github/workflows/release.yml` (build stage) builds and packages binaries for:

- `x86_64-unknown-linux-gnu` (`linux_amd64`)
- `x86_64-apple-darwin` (`darwin_amd64`)
- `aarch64-apple-darwin` (`darwin_arm64`)

Each release tarball now follows the SDK layout contract:

```text
bin/plasmite
bin/pls
include/plasmite.h
lib/libplasmite.(dylib|so)
lib/libplasmite.a               # optional
lib/pkgconfig/plasmite.pc
```

`release.yml` uploads build artifacts only (SDK tarballs, Python dist artifacts, npm tarball, and release metadata).

`.github/workflows/release-publish.yml` (publish stage) consumes a successful build run's artifacts, runs registry preflight checks, publishes crates/npm/PyPI, and then creates/updates the GitHub release with SDK tarballs + `sha256sums.txt`.

Before any registry publish steps run, `release-publish.yml` now also verifies Homebrew tap alignment (version + URLs + checksums). If tap alignment is stale, publish fails closed before crates/npm/PyPI.

For low-risk workflow validation after release workflow changes, run a no-publish rehearsal:

```bash
gh workflow run release-publish.yml -f build_run_id=<successful-release-build-run-id> -f rehearsal=true
```

If publish fails due to registry credentials, rerun only publish without rebuilding matrix artifacts:

```bash
gh workflow run release-publish.yml -f build_run_id=<successful-release-build-run-id> -f rehearsal=false -f allow_partial_release=false
```

## Performance monitoring policy

- Release-blocking performance checks are local-only and run on the maintainer host with the same power/runtime conditions for baseline and candidate.
- Use:
  - `bash skills/plasmite-release-manager/scripts/compare_local_benchmarks.sh --base-tag <vX.Y.Z> --runs 3`
- CI benchmark monitoring is advisory and non-blocking via `.github/workflows/perf-monitor.yml` (scheduled + manual runs, artifact capture only).
- Multi-platform performance sweeps are optional and should be run when platform-sensitive code changes (I/O, mmap, locking, FFI/bindings), not required for every patch release.

## Linux arm64 policy

- `aarch64-unknown-linux-gnu` is currently best-effort.
- It is not a release-gating target in `release.yml` or `release-publish.yml`.
- ARM64 Linux users should build from source unless/until gated support is reintroduced.
