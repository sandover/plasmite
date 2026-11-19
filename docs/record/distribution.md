# Distribution

This document says what each install channel provides, which platforms each supports, and how the SDK archive is laid out.

`release/targets.json` owns the native target identifiers and tiers for the channels in the release matrix. `scripts/validate_distribution_targets.sh` checks the official target list and the cargo-binstall preview list against the manifest.

## Support tiers

A platform and channel combination is `official` when all of these hold:

- Users have a documented install command or artifact, with explicit prerequisites. Native binary channels need no source build; Rust and Go channels use their standard source toolchains.
- Automated smoke checks in CI or the release workflows exercise the install path.
- The install matrix below marks it `official`.

A combination is `preview` when its artifacts and install metadata exist and at least one release-time smoke check covers it, but coverage of its full target matrix is incomplete. Promotion to `official` needs a change to `release/targets.json` and this document, plus delivery evidence; publishing a preview artifact does not promote it.

## Platforms

Official platforms:
- macOS: `aarch64-apple-darwin`, `x86_64-apple-darwin`
- Linux: `x86_64-unknown-linux-gnu`
- Windows: `x86_64-pc-windows-msvc` via npm and PyPI release artifacts

Not currently targeted:
- Linux distro packages (`apt`, `yum`, `pacman` and so on), musl and Alpine, ARMv6, and native Windows ARM64. An ARM64 Windows machine runs the x64 build through Windows' emulation.

**Linux x86_64.** Ubuntu 22.04 runners build the release SDK, npm native assets and Python wheels against glibc 2.35. Native installs need glibc 2.35 or newer, as on Ubuntu 22.04 and Debian 12. The wheel smoke checks the `manylinux_2_35_x86_64` platform tag and bundled binaries. Check your system with `getconf GNU_LIBC_VERSION`.

**macOS.** Native artifacts target macOS 10.12 on Intel and macOS 11 on Apple Silicon. Python wheel tags state the architecture and minimum OS of the extension, bundled CLI and library; a single-architecture build does not claim `universal2`. These are compiler deployment targets: release smoke tests run only on the CI hosts, not on every older macOS release.

**Linux ARM preview.** The GitHub release includes SDK archives for two more targets:

- Linux ARM64: `aarch64-unknown-linux-gnu` (`linux_arm64`), ARMv8-A, built natively on Ubuntu 22.04 for ARM64.
- Linux ARMv7, 32-bit hard-float: `armv7-unknown-linux-gnueabihf` (`linux_armv7`), cross-built with GCC's `arm-linux-gnueabihf` compiler against an Ubuntu 22.04 sysroot. It requires Armv7-A with VFPv3-D16 and Thumb-2 (`-march=armv7-a -mfpu=vfpv3-d16 -mfloat-abi=hard`); NEON is optional.

Both need glibc 2.35 or newer. Each archive holds the CLI, server and C SDK, so you can run Plasmite without Rust; npm, PyPI and Homebrew do not cover ARM Linux. Release CI smoke-tests each packaged archive, runs the core and server test suites on each target, and hands a pool between 64-bit and ARMv7 CLIs; ARMv7 runs under QEMU. No check on physical Raspberry Pi hardware has passed yet, so both targets stay in preview. The [ARM SDK CI report](../validation/arm-sdk-ci.md) records the first run.

On ARMv7 a pool file can be at most 2,147,483,647 bytes (2 GiB minus one byte). Plasmite refuses to create or map a larger pool. A smaller pool can still fail to map when memory is short.

## Install matrix

| Channel | Install Command | Provides CLI | Provides Library | Tier | Notes |
| --- | --- | --- | --- | --- | --- |
| Homebrew (macOS and x86_64 Linux) | `brew install sandover/tap/plasmite` | Yes | Yes (system SDK) | `official` | Installs `bin/`, `lib/`, `include/` and pkg-config metadata. |
| crates.io (Rust) | `cargo install plasmite` | Yes | No | `official` | Builds from source into Cargo's bin directory. |
| crates.io (Rust) | `cargo add plasmite` | No | Yes (Rust crate) | `official` | Standard Rust dependency. |
| PyPI (Python) | `uv tool install plasmite` | Yes | Yes (Python bindings) | `official` (macOS x64/arm64; Linux/Windows x64) | Wheels bundle the native library and CLI. |
| npm (Node) | `npm i -g plasmite` | Yes | Yes (Node bindings) | `official` (macOS x64/arm64; Linux/Windows x64) | Bundles the add-on, native library and CLI. |
| Go module | `go get github.com/sandover/plasmite/bindings/go/v2/local@v2.0.0` | No | Yes (Go bindings) | `official` (macOS/Linux) | Install the 2.x SDK through Homebrew or an SDK archive for cgo. Import pure contracts from `github.com/sandover/plasmite/bindings/go/v2/api` to build without cgo. |
| GitHub release archive | Download from releases | Yes | Yes (SDK layout) | `official` (macOS, Linux x86_64) | Contains `bin/`, `lib/`, `include/` and `lib/pkgconfig/`. |
| GitHub release archive (Linux ARM64) | Download `plasmite_<version>_linux_arm64.tar.gz` | Yes | Yes (SDK layout) | `preview` | See the ARM preview above. |
| GitHub release archive (Linux ARMv7) | Download `plasmite_<version>_linux_armv7.tar.gz` | Yes | Yes (SDK layout) | `preview` | See the ARM preview above. |
| cargo-binstall (Rust binary installer) | `cargo binstall plasmite --no-confirm` | Yes | No | `preview` (`x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`) | Maps to the GitHub release archives through `package.metadata.binstall`; the release-publish smoke gate is currently Linux-only. |

The npm package needs Node.js 24 or newer. The Python package needs Python 3.10 or newer. Go code needs Go 1.22 or newer, and Go's local and UDP packages also need cgo, a C compiler and pkg-config. Rust source installs need the toolchain and C compiler in the [build guide](../building.md#install-the-cli-from-source).

Read the [2.0 upgrade guide](upgrading-2.0.md) before replacing a 1.x installation.

## Native library versions

Install bindings and the native library from the same major version. The library's file name carries the major version, so a 1.x and a 2.x library can sit side by side: `libplasmite.so.2` on Linux, `libplasmite.2.dylib` on macOS and `plasmite2.dll` on Windows. Python, Go and the Node add-on check that the loaded library reports major version 2 and refuse anything else. The npm package places the library beside the add-on.

## Language notes

**Node.** The HTTP `RemoteClient` works without the native add-on: the npm package includes a checked WebAssembly core for encoding and validating messages. Local pools and UDP still need the add-on. The WebAssembly backend supports the Node entry point, not browsers, and compiles its module on first use.

**Python.** Message views use a native CPython extension over the C ABI. Releases ship `cp311-abi3` wheels for CPython 3.11 and newer, `cp310-cp310` wheels for CPython 3.10 (whose buffer protocol needs a separate build), and platform-specific `py3-none` fallback wheels. PyPy and free-threaded CPython use the same view API through ctypes, with extra copies and slower field access; free-threaded Python skips the extension so the GIL stays disabled. The release wheel smoke builds all three variants from the source archive, checks their native contents and tags, and installs and exercises each one. The [Python binding guide](../../bindings/python/README.md#advanced) explains how the package finds its library and builds wheels.

On a platform without a compatible wheel, Python's source package needs a C compiler and a separate system SDK. For an extracted SDK archive, point `PLASMITE_LIB_DIR` at its `lib` directory and put its `bin` directory on `PATH`.

## Linux ARM SDK preview install

On a Raspberry Pi, use `getconf LONG_BIT` to check whether the installed OS runs 32-bit or 64-bit programs. For a 32-bit OS, confirm that the CPU and OS meet the ARMv7 hard-float requirements above. The intended 32-bit system is a Raspberry Pi 2 running 32-bit Raspberry Pi OS Lite. Find the archive and `sha256sums.txt` on the [release page](https://github.com/sandover/plasmite/releases).

This script downloads the right archive, stops if the download or checksum fails, and extracts it:

```bash
set -euo pipefail
case "$(getconf LONG_BIT)" in
  32) platform=linux_armv7 ;;
  64) platform=linux_arm64 ;;
  *) echo "unsupported userland width" >&2; exit 1 ;;
esac
getconf GNU_LIBC_VERSION # Requires glibc 2.35 or newer.
version=2.0.0
archive="plasmite_${version}_${platform}.tar.gz"
release_url="https://github.com/sandover/plasmite/releases/download/v${version}"
mkdir -p "$HOME/arm-sdk-${version}"
cd "$HOME/arm-sdk-${version}"
curl -fL "$release_url/$archive" -o "$archive"
curl -fL "$release_url/sha256sums.txt" -o sha256sums.txt
awk -v archive="$archive" '$2 == "./" archive || $2 == archive { print; found = 1 } END { if (!found) exit 1 }' sha256sums.txt |
  sha256sum --check -
sdk_dir="$HOME/.local/share/plasmite/${version}"
mkdir -p "$sdk_dir"
tar -xzf "$archive" -C "$sdk_dir"
export PATH="$sdk_dir/bin:$PATH"
plasmite --version
```

The archive extracts `bin/`, `include/` and `lib/` into `sdk_dir`. Add the `bin` path to your shell's startup file for later sessions. For C programs, set `PKG_CONFIG_PATH="$sdk_dir/lib/pkgconfig"` and use `pkg-config --cflags --libs plasmite`; to run a dynamically linked program, also add `$sdk_dir/lib` to `LD_LIBRARY_PATH`. A Python source install can use `PLASMITE_LIB_DIR="$sdk_dir/lib"` with the same `bin` path on `PATH`.

Run a foreground server as the account that owns the pool directory; installed servers on Linux use that account too. Follow the [serving guide](serving.md#share-your-first-pool) to set up and run a server.

## cargo-binstall promotion criteria

cargo-binstall stays in preview until all of these hold:

- Release automation smoke-tests every intended official target triple, not only Linux, and fails closed.
- Post-release delivery checks cover the same targets.
- This document and the [release policy](releasing.md) record the new tier.

## SDK layout

GitHub release archives define the SDK layout:

```text
bin/
  plasmite
  pls
include/
  plasmite.h
lib/
  libplasmite.so.2 or libplasmite.2.dylib
  libplasmite.a
  pkgconfig/
    plasmite.pc
LICENSE
THIRD_PARTY_NOTICES.md
```

### pkg-config contract

The `plasmite.pc` file must:

- Be named `plasmite` (not `libplasmite`).
- Provide `Cflags: -I...` for `include/plasmite.h`.
- Link the shared library for the current major version: `-L... -l:libplasmite.so.2` on Linux or `-L... -lplasmite.2` on macOS.
- On Linux, provide `Libs.private: -lpthread -ldl -lm` for consumers of `libplasmite.a`.

To link the static library, pass the path to `libplasmite.a` explicitly and include the system libraries for your platform. `pkg-config --static --libs plasmite` adds the private dependencies to the same shared-library link line.
