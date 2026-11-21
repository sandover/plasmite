# Building Plasmite

## What gets built

- The `plasmite` crate: the Rust library, the `plasmite` and `pls` binaries, and the C library. `Cargo.toml` builds the library as an `rlib` for Rust, a shared library for the bindings and a static archive for C programs.
- Vendored Lite3 sources under `vendor/lite3/`, compiled by `build.rs`.
- `c/lite3_shim.c` and `c/lite3_builder.c`, the C helpers Rust uses to read and build Lite3 values.
- `c/notify_shim.c`, the macOS wake-up bridge, which uses shared-address waits only where the SDK and OS provide them.
- The web UI font, vendored Inconsolata under `ui/fonts/` and embedded in the server binary (source and checksum in `ui/fonts/README.md`).

Cargo names the shared library `libplasmite.so`, `libplasmite.dylib` or `plasmite.dll`. The bindings load the major-versioned name instead (`libplasmite.so.2`, `libplasmite.2.dylib`, `plasmite2.dll`), so `just abi` and the binding test recipes stage that name next to Cargo's output with `scripts/stage_versioned_library.sh`. [Releasing](record/releasing.md#native-library-major-version) explains the versioning.

## Install the CLI from source

Use a source build to try changes that have not reached a release. You need Git, the Rust toolchain pinned in `rust-toolchain.toml` and a C compiler. From a checkout, run:

```console
cargo install --path . --locked --force --bin plasmite
plasmite --version
```

This replaces a Cargo-managed Plasmite installation. If `plasmite --version` still shows an older version, an npm, Python or Homebrew install comes first on your `PATH`; put Cargo's bin directory ahead of it.

On Windows, see [Windows builds](#windows-builds).

## Lite3 vendoring

Plasmite pins Lite3 to an exact upstream commit in `vendor/lite3.lock.json`. `vendor/lite3.sha256` holds checksums for the vendored files and the local read-safety patch. Builds and integrity checks use the local Lite3 snapshot. Cargo and Rustup can still need network access to fetch uncached crates and toolchains.

`build.rs` tracks the vendored files and local C helpers, then uses the `cc` crate to compile them into one static archive. On macOS it also compiles `c/notify_shim.c` into `libplasmite_notify.a`. A missing source file stops compilation. An empty or incomplete file can leave unresolved `lite3_*` symbols at link time.

The macOS bridge weak-links the shared-address wait calls and uses them only on macOS 14.4 or later; an SDK without the header compiles a stub. Its deployment target follows `MACOSX_DEPLOYMENT_TARGET`, defaulting to macOS 10.12 on Intel and 11.0 on Apple Silicon, so the availability checks hold even when the SDK targets a newer OS. Older systems fall back to semaphores and polling with the same file format.

Check the snapshot's metadata, file set and checksums:

```bash
just verify-lite3
```

To adopt a new upstream revision, pick a full commit SHA and run:

```bash
just update-lite3 <40-character-commit>
```

The updater rebuilds `vendor/lite3/` from a fixed allowlist, applies `vendor/lite3-read-safety.patch` and updates the provenance and checksums. The patch checks node bounds and alignment before the compiler assumes alignment, and keeps iterator prefetch indexes inside their seven-slot array. If the patch no longer applies, the updater stops before replacing the snapshot. Retire the patch when upstream ships an equivalent fix. The updater fetches the selected commit from upstream.

Scheduled CI reports new commits on Lite3's `main` branch. Run the same check on demand with `just check-lite3-upstream`. On Linux with Clang, `just test-lite3-sanitizers` builds the C sources with AddressSanitizer and UndefinedBehaviorSanitizer and runs the Lite3 tests and C append checks at `-O0` and `-O2`, as CI does.

## Build profiles and disk use

Development and test builds keep line-table debug information for Plasmite and drop it for dependencies, which preserves useful backtraces without paying to build and link dependency debug data. For full variable inspection, set `CARGO_PROFILE_DEV_DEBUG=2` or `CARGO_PROFILE_TEST_DEBUG=2` for that session.

Cargo never deletes builds it replaces, so `target/` grows with each change to dependencies, flags or toolchain. A clean build of everything `just check` needs is about 1 GB, so `just check` runs `cargo clean` first when `target/` passes 2 GB.

[Testing](record/testing.md) says which checks to run before you push, merge and release.

## Local developer server

`just serve-dev` builds the CLI, seeds a demo pool under `/tmp/plasmite-dev/pools` and starts the browser interface at `http://127.0.0.1:9009/ui`. Its HTTPS listener takes an ephemeral loopback port so it never collides with another server. Use `just serve-status` and `just serve-log` to inspect it and `just serve-stop` to stop it. These recipes stop only the process they started, after checking its executable, pool directory and listener arguments; a stale PID that now names another process causes a refusal.

To exercise HTTPS access, find the dev server's HTTPS address with `./target/debug/plasmite serve status`, then create a key. Replace `HTTPS_PORT` with the port from the status output.

```bash
./target/debug/plasmite --dir /tmp/plasmite-dev/pools access invite laptop
# On the client, paste the key at the hidden prompt:
./target/debug/plasmite access connect https://127.0.0.1:HTTPS_PORT
```

`access invite` prints the key; keep it private.

For a server that lasts only as long as one command, use `just serve-with '<command>'`. Both recipes use the fixed port 9009, report a startup failure if another process holds it, and wait up to ten seconds for the server's ready file before running anything. An early exit or timeout prints the server log and cleans up.

To review command help, human output, JSON output, finite history and recovery errors in one pass, run:

```bash
cargo build --bin plasmite
bash scripts/cli_ux_tour.sh
```

The tour logs to `tmp/cli-ux-tour.log` and removes its temporary pool and access directories when it finishes. Its `serve status` step reads the current user's server registry.

## Python tooling

Use `uv` for every Python environment and package operation (`uv venv`, `uv pip`, `uv tool`, `uv run`). Do not add `pip` commands to docs or runbooks.

## SDK archives

To link a C program against `include/plasmite.h`, build a release-style SDK archive from source:

```bash
just sdk-from-source                       # on Linux x86_64
just sdk-from-source aarch64-apple-darwin  # on Apple Silicon
```

This builds `plasmite`, `pls` and the shared and static libraries, creates `dist/plasmite_<version>_<platform>.tar.gz` in the [SDK layout](record/distribution.md#sdk-layout), and checks the extracted archive. The smoke check runs the target binaries, so use a host that can run them. To use the shared library through pkg-config, replace `VERSION` with the package version and use the archive suffix for your target.

```bash
mkdir -p /path/to/sdk
tar -xzf dist/plasmite_VERSION_linux_amd64.tar.gz -C /path/to/sdk
export PKG_CONFIG_PATH=/path/to/sdk/lib/pkgconfig
pkg-config --cflags --libs plasmite
```

`scripts/build_release_artifacts.sh <target-triple> [--static]` builds the native artifacts once per target. The release workflow packages the SDK, Python wheels and Node native assets from that output for the channels each target supports. [Releasing](record/releasing.md) and the [release runbook](../skills/plasmite-release-manager/SKILL.md) cover publication. [Distribution](record/distribution.md) lists the targets, platform baselines and the Linux ARM preview.

To check a Linux x86_64 wheel against the glibc 2.35 baseline, build the SDK on that baseline and run:

```bash
PLASMITE_SDK_DIR=/path/to/sdk \
PLASMITE_MANYLINUX_PLATFORM=manylinux_2_35_x86_64 \
  bash scripts/python_wheel_smoke.sh
```

The check uses `auditwheel` and rejects a newer glibc dependency instead of only renaming the wheel. Without these variables the script runs the ordinary local wheel smoke.

### Release targets

Edit [release/targets.json](../release/targets.json) to change the targets,
runners, or artifacts that the release workflows build. The
[distribution guide](record/distribution.md) states the support tier for each
channel. Run `just check-release-targets` after a change to check the manifest,
the support policy, and the workflow scripts that use it.

## Windows builds

Build in an x64 Visual Studio developer terminal with `clang-cl` on `PATH`, the Visual Studio C++ linker and the Windows SDK. Use the x64 Rust and LLVM toolchains, including on ARM64 Windows, where the build runs under emulation. `cl.exe` cannot build the vendored C code, which uses compiler extensions it lacks.

Keep server state and saved connections on a filesystem that enforces Windows access control lists, such as NTFS. [Testing](record/testing.md#windows) lists the Windows-only test commands.

Troubleshooting:

- **`cl.exe` errors such as `__builtin_expect`, `__attribute__` or parse errors in `lite3.h`.** The build is using `cl.exe`; put `clang-cl` on `PATH`, or install through `uv tool install plasmite` or `npm i -g plasmite` instead.
- **Lite3 parse errors near `case` labels.** Run `just verify-lite3`; Plasmite pins a C11-compatible revision, so these errors point to a modified or incomplete snapshot.
- **`feed` fails with `failed to encode json as lite3`.** After `access connect`, feed through the HTTPS pool URL so the server encodes the message. See [secure sharing](record/serving.md#share-your-first-pool).

## Portable Node view backend

The Node package's HTTP client includes a checked WebAssembly (Wasm) core built from the shared Rust and C reader and input builder, so remote-only users need no native add-on or compiler. Maintainers build it with the pinned Rust toolchain and WASI SDK 34; see the [portable core build instructions](../bindings/node/portable-core/README.md). `npm run check:portable` verifies the artifact, its import-free interface and the shared-source manifest without building native code.
