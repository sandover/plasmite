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

## Release artifact matrix

`.github/workflows/release.yml` builds and packages binaries for:

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

The release workflow uploads SDK tarballs and `sha256sums.txt` to the GitHub release.

## Linux arm64 policy

- `aarch64-unknown-linux-gnu` is currently best-effort.
- It is not a release-gating target in `release.yml`.
- ARM64 Linux users should build from source unless/until gated support is reintroduced.
