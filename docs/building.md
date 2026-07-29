# Building Plasmite

## What gets built

- Rust crate: `plasmite` (CLI, library, tests, bindings support)
- Native C dependency: vendored Lite3 sources under `vendor/lite3/`
- C shim: `c/lite3_shim.c` exports the narrow ABI used by Rust FFI

`Cargo.toml` declares `build = "build.rs"`, so Cargo always runs the build script when needed.

## Native build model (Lite3 vendoring)

Plasmite pins Lite3 to an exact upstream commit in
`vendor/lite3.lock.json`. An integrity manifest at `vendor/lite3.sha256`
covers the curated files compiled into Plasmite. Normal builds and integrity
checks never access the network.

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

Verify the snapshot metadata, complete file set, and checksums:

```bash
just verify-lite3
```

To review and adopt a new upstream revision, select a full commit SHA and run:

```bash
just update-lite3 <40-character-commit>
```

The update command reconstructs `vendor/lite3/` from a fixed allowlist and
updates both provenance and integrity metadata. It accesses the network; normal
builds and `just check` do not.

Scheduled CI detects new commits on Lite3's `main` branch. Run the same
networked check on demand with:

```bash
just check-lite3-upstream
```

Linux CI also compiles the vendored C sources with AddressSanitizer and
UndefinedBehaviorSanitizer, links their runtimes into the Rust test binary, and
runs the focused Lite3 suite. Reproduce that environment on a Linux host with
Clang by running:

```bash
just test-lite3-sanitizers
```

## Local validation gates

Use Cargo's fastest applicable command while developing. Start with a type and
borrow check, then run the smallest test target that covers the changed
behavior:

```bash
cargo check
cargo test --lib <module-or-test-name>
cargo test --test <integration-test-name> <test-name>
```

Development and test profiles emit line-table debug information. This preserves
source locations and useful backtraces without paying for full debugger variable
metadata. A debugging session that needs full variable inspection can override
the profile temporarily with `CARGO_PROFILE_DEV_DEBUG=2` or
`CARGO_PROFILE_TEST_DEBUG=2`.

Run the complete local gate when work is ready for handoff or push:

```bash
just check          # formatting, linting, Rust tests, version checks, Lite3 integrity
just integration    # bindings, ABI, cookbook, and cross-artifact checks
just release-gate   # check + integration + Python wheel smoke
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
- `x86_64-pc-windows-msvc` (`windows_amd64` for Python and `win32-x64` for Node)

Each release tarball now follows the SDK layout contract:

```text
bin/plasmite
bin/pls
include/plasmite.h
lib/libplasmite.(dylib|so)
lib/libplasmite.a               # optional
lib/pkgconfig/plasmite.pc
```

## Source SDK build (C `libplasmite` consumers)

If you want to link a C program against `include/plasmite.h` and `libplasmite`,
build a local SDK tarball from source in release-style layout:

```bash
just sdk-from-source
```

Default output:

```text
dist/plasmite_<version>_linux_amd64.tar.gz
```

This command builds `plasmite` + `pls`, builds shared/static `libplasmite`,
packages `bin/`, `include/`, `lib/`, `lib/pkgconfig/plasmite.pc`, and runs
artifact smoke checks.

`scripts/build_release_artifacts.sh <target-triple> [--static]` is the shared
release build entrypoint. The release workflow invokes it once per platform,
then reuses those artifacts for the SDK, Python wheel, and Node package instead
of maintaining separate Cargo command blocks for each distribution channel.

Use the SDK from your C build via `pkg-config`:

```bash
tar -xzf dist/plasmite_<version>_linux_amd64.tar.gz -C /path/to/sdk
export PKG_CONFIG_PATH=/path/to/sdk/lib/pkgconfig
pkg-config --cflags --libs plasmite
```

For static linking on Linux:

```bash
pkg-config --cflags --static --libs plasmite
```

You can override target/platform tags:

```bash
just sdk-from-source aarch64-apple-darwin
```

`release.yml` uploads build artifacts only (SDK tarballs, Python dist artifacts, npm tarball, and release metadata).

`.github/workflows/release-publish.yml` (publish stage) consumes a successful build run's artifacts, runs registry preflight checks, syncs/verifies the Homebrew tap formula, publishes crates/npm/PyPI, and then creates/updates the GitHub release with SDK tarballs + `sha256sums.txt`.

Before any registry publish steps run, `release-publish.yml` verifies that the independently maintained `sandover/homebrew-tap` formula is aligned with the build artifacts (version + URLs + checksums). Update and push that formula locally before a live publish; CI never mutates tap history.

After publishing, dispatch `post-release-smoke.yml`. Its macOS Homebrew job installs `sandover/tap/plasmite` and verifies `plasmite --version` for the released version.

For low-risk workflow validation after release workflow changes, run a no-publish rehearsal:

```bash
gh workflow run release-publish.yml -f release_tag=<vX.Y.Z> -f rehearsal=true
```

If publish fails due to registry credentials, rerun only publish without rebuilding matrix artifacts:

```bash
gh workflow run release-publish.yml -f release_tag=<vX.Y.Z> -f rehearsal=false
```

If you need to force a specific build run (for example, during incident recovery), you can still pass `build_run_id` instead of `release_tag`.

## Performance monitoring policy

- Release-blocking performance checks are local-only and run on the maintainer host with the same power/runtime conditions for baseline and candidate.
- Use:
  - `bash skills/plasmite-release-manager/scripts/compare_local_benchmarks.sh --base-tag <vX.Y.Z> --runs 3`
- Multi-platform performance sweeps are optional and should be run when platform-sensitive code changes (I/O, mmap, locking, FFI/bindings), not required for every patch release.

## Linux arm64 policy

- `aarch64-unknown-linux-gnu` is currently best-effort.
- It is not a blocking CI target in `.github/workflows/ci.yml`.
- It is not built or published in the blocking release matrix in `.github/workflows/release.yml`.
- It is not a release-gating target in `release-publish.yml`.
- ARM64 Linux users should build from source unless/until gated support is reintroduced.

## Windows support policy

- Windows (`x86_64-pc-windows-msvc`) is now an official release channel for:
  - Python wheel delivery (`windows_amd64`)
  - Node native delivery (`win32-x64`)
- These channels are built and smoke-tested in `release.yml` and published through `release-publish.yml`.
- Windows rollback-only fallback workflows have been removed; official Windows delivery is via Python/Node release channels.

## Windows troubleshooting

- **Source build fails with `cl.exe` errors (`__builtin_expect`, `__attribute__`, parsing errors in `lite3.h`)**
  - Prefer official install channels (`uv tool install plasmite`, `npm i -g plasmite`) over local source builds.
- **Source build fails with Lite3 parse errors near `case` labels**
  - Verify the vendored snapshot with `just verify-lite3`.
  - Plasmite pins a C11-compatible Lite3 revision; unexpected parse errors can indicate a modified or incomplete snapshot.
- **`feed` fails with `failed to encode json as lite3`**
  - Use remote refs (`http://host:port/<pool>`) so encoding occurs on the remote server.
- **Emergency fallback artifact integrity**
  - PowerShell: `Get-FileHash .\\plasmite_<version>_windows_amd64_preview.zip -Algorithm SHA256`
  - Compare with the accompanying `.sha256` file.
