# Building Plasmite

## What gets built

- Rust crate: `plasmite` (CLI, library, tests, bindings support)
- Native C dependency: vendored Lite3 sources under `vendor/lite3/`
- C shim: `c/lite3_shim.c` exports the narrow ABI used by Rust FFI
- Web UI font: vendored Inconsolata under `ui/fonts/`, embedded in the server binary (source and checksum in `ui/fonts/README.md`)

`Cargo.toml` declares `build = "build.rs"`, so Cargo always runs the build script when needed.

## Install the CLI from source

Use a source build to try changes that have not reached a published release.
You need Git, the Rust toolchain pinned in `rust-toolchain.toml`, and a C
compiler. From a checkout of this repository, run:

```console
cargo install --path . --locked --force --bin plasmite
plasmite access --help
```

This replaces a Cargo-managed Plasmite installation. Cargo installs the
binary in its bin directory. Put that directory first on `PATH` if your
shell finds an older npm, Python, or Homebrew installation. If `access`
still fails, run the binary from Cargo's bin directory directly.

On Windows, use an x64 Visual Studio developer terminal with `clang-cl` on
`PATH`, the Visual Studio C++ linker, and the Windows SDK. Use the x64 Rust
toolchain for the supported Windows build, including on ARM64 Windows.
See [Windows support](#windows-support-policy) for the target and checks.

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

Development and test profiles emit line-table debug information for Plasmite
and omit debug information from dependencies. This preserves source locations
and useful backtraces in Plasmite without paying to generate or link dependency
debug metadata. A debugging session that needs full variable inspection can
override the profile temporarily with `CARGO_PROFILE_DEV_DEBUG=2` or
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
- `aarch64-unknown-linux-gnu` (`linux_arm64`)
- `armv7-unknown-linux-gnueabihf` (`linux_armv7`)
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

The planned Linux ARM archives also include the `plasmite` and `pls`
executables, so you can run the CLI and server without installing Rust. Once a
release publishes these preview archives, download the one for your Raspberry
Pi's architecture, extract it, and put its `bin/` directory on `PATH`:

```bash
mkdir -p "$HOME/.local/share/plasmite"
tar -xzf plasmite_<version>_linux_arm64.tar.gz -C "$HOME/.local/share/plasmite"
export PATH="$HOME/.local/share/plasmite/bin:$PATH"
plasmite --version
```

Use `linux_armv7` for 32-bit ARM Linux with the hard-float ABI. For a server,
run Plasmite as the operating-system account that owns its pool directory, and
keep that directory private to the service. Start with the
[serving guide](record/serving.md#share-your-first-pool) for server setup and
pool ownership; this archive does not prescribe a service manager. These
archives are GitHub SDK previews. This change configures their release builds
and smoke checks; it does not publish them. The configured checks do not
replace testing on physical Raspberry Pi hardware or published-release
verification.

On ARMv7, a pool file can be at most 2,147,483,647 bytes (2 GiB minus one
byte). Plasmite rejects creation of a larger pool or mapping of a larger
existing pool. A smaller pool can still fail to map if the operating system
lacks memory.

The ARM64 build uses Ubuntu 22.04 on ARM64, with glibc 2.35 as its intended
runtime baseline. The ARMv7 build uses GCC's `arm-linux-gnueabihf` cross
compiler with an Ubuntu 22.04 glibc 2.35 sysroot. Its compiler flags pin
`-march=armv7-a -mfpu=vfpv3-d16 -mfloat-abi=hard`; the Rust target also uses
Thumb-2; dependencies can select optional NEON routines at runtime. These are configured build baselines; live
runner smoke and runtime compatibility on target systems remain pending.

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

## Linux ARM archive preview

- `aarch64-unknown-linux-gnu` (`linux_arm64`) and
  `armv7-unknown-linux-gnueabihf` (`linux_armv7`) are GitHub SDK preview
  targets for Raspberry Pi CLI/server use.
- The archives contain the CLI and SDK layout. Users can install and run them
  without Rust.
- The release workflow is configured to build and smoke-test both targets.
  Physical Raspberry Pi testing and published-release verification remain
  separate evidence and must be recorded before either is claimed complete.
- ARMv6 is outside this preview. npm, PyPI, and Homebrew remain unchanged.

## Windows support policy

- Windows (`x86_64-pc-windows-msvc`) is now an official release channel for:
  - Python wheel delivery (`windows_amd64`)
  - Node native delivery (`win32-x64`)
- These channels are built and smoke-tested in `release.yml` and published through `release-publish.yml`.
- Windows rollback-only fallback workflows have been removed; official Windows delivery is via Python/Node release channels.

Windows CI also runs secure-sharing tests against the release CLI: private
state permissions, native connections, browser sessions, direct MCP
authorization, restart, and revocation. To run that check from PowerShell:

```powershell
cargo test --release --lib windows_private::tests -- --test-threads=1
cargo test --release --test secure_sharing --test access_lifecycle --test browser_access --test oauth_access -- --test-threads=1
```

Use a filesystem that enforces Windows access control lists, such as NTFS, for
server state and saved connections. The official artifact targets Windows
x86_64; an ARM64 Windows VM can run it through Windows' x64 emulation.

Source builds require `clang-cl`, the Visual Studio C++ linker, and the Windows
SDK. The x86_64 Rust and LLVM toolchains also build the supported target inside
an ARM64 Windows VM. The vendored C code uses extensions that `cl.exe` does not
support.

## Windows troubleshooting

- **Source build fails with `cl.exe` errors (`__builtin_expect`, `__attribute__`, parsing errors in `lite3.h`)**
  - Prefer official install channels (`uv tool install plasmite`, `npm i -g plasmite`) over local source builds.
- **Source build fails with Lite3 parse errors near `case` labels**
  - Verify the vendored snapshot with `just verify-lite3`.
  - Plasmite pins a C11-compatible Lite3 revision; unexpected parse errors can indicate a modified or incomplete snapshot.
- **`feed` fails with `failed to encode json as lite3`**
  - After `access connect`, use an HTTPS pool URL so the remote server encodes the message. See [secure sharing](record/serving.md#share-your-first-pool).
- **Emergency fallback artifact integrity**
  - PowerShell: `Get-FileHash .\\plasmite_<version>_windows_amd64_preview.zip -Algorithm SHA256`
  - Compare with the accompanying `.sha256` file.
