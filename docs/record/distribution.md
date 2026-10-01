# Distribution

This document defines what users get from each install channel, supported platforms, and the stable on-disk SDK layout.

Native target identifiers and per-target channel tiers are owned by
`release/targets.json`. This document explains the support policy in human
terms and intentionally repeats important identifiers so readers do not need to
inspect build data. `scripts/validate_distribution_targets.sh` fails when those
lists drift apart.

## Support Tiers

A platform/channel combination is `official` only when all of these are true:
- Users have a documented install command or artifact path, with explicit
  prerequisites. Native binary channels require no source build; Rust and Go
  channels use their standard source toolchains.
- The install path is exercised by automated smoke checks in CI/release workflows.
- The combination is explicitly marked `official` in the install matrix.

A combination can be `preview` when install metadata/artifacts are wired and at least one release-time smoke gate exists, but full target-matrix coverage is not yet complete.

Official platforms:
- macOS: `aarch64-apple-darwin`, `x86_64-apple-darwin`
- Linux: `x86_64-unknown-linux-gnu`
- Windows: `x86_64-pc-windows-msvc` via npm and PyPI release artifacts

The 1.0 Linux x86_64 prebuilt CLI (SDK, Homebrew, and npm) requires glibc
2.39 or newer. Ubuntu 22.04 and Debian 12 have older glibc versions; use a
source build on those systems. The ARM preview has its separate glibc 2.35
baseline below. Check your system with `getconf GNU_LIBC_VERSION`.

macOS native artifacts target macOS 10.12 on Intel and macOS 11 on Apple
Silicon. Python wheel tags describe the bundled CLI and library architecture
and their minimum OS version; a thin native bundle does not claim `universal2`.
These compiler deployment targets do not establish runtime testing on every
older macOS version. Release smoke tests run on the declared CI hosts.

Not currently targeted:
- Linux distro packages (`apt`, `yum`, `pacman`, etc.)

GitHub SDK preview platforms:
- Linux ARM64: `aarch64-unknown-linux-gnu` (`linux_arm64`)
- Linux ARMv7, 32-bit hard-float: `armv7-unknown-linux-gnueabihf` (`linux_armv7`)

These preview archives provide the CLI/server and SDK layout for manual
installation without Rust. CI builds and smoke-tests both targets. ARM64 uses
Ubuntu 22.04 with glibc 2.35 as its runtime baseline. ARMv7 uses Armv7-A,
VFPv3-D16 hard-float, and Thumb-2, with NEON optional, against an Ubuntu 22.04
glibc 2.35 sysroot. Physical Raspberry Pi compatibility remains to be checked,
and no GitHub release has published these assets yet. ARMv6 is outside this
preview.

The passing [ARM SDK CI run](https://github.com/sandover/plasmite/actions/runs/36803923793)
retains the archives for 30 days as `ci-sdk-aarch64-unknown-linux-gnu` and
`ci-sdk-armv7-unknown-linux-gnueabihf`. Each artifact includes the SDK tarball,
`sha256sums.txt`, and `ci-build.json` provenance. The CI archive filenames use
`plasmite_<version>_linux_arm64.tar.gz` and
`plasmite_<version>_linux_armv7.tar.gz`, which are also the planned GitHub
release asset names.

## Install Matrix

| Channel | Install Command | Provides CLI | Provides Library | Tier | Notes |
| --- | --- | --- | --- | --- | --- |
| Homebrew (macOS and x86_64 Linux) | `brew install sandover/tap/plasmite` | Yes | Yes (system SDK) | `official` | Installs `bin/`, `lib/`, `include/`, `pkg-config` metadata; post-release macOS install smoke required. |
| crates.io (Rust) | `cargo install plasmite` | Yes | No | `official` | Installs binaries into Cargo bin dir; source build. |
| crates.io (Rust) | `cargo add plasmite` | No | Yes (Rust crate) | `official` | Standard Rust dependency. |
| PyPI (Python) | `uv tool install plasmite` | Yes | Yes (Python bindings) | `official` (macOS x64/arm64; Windows x64) | Wheel bundles native assets and CLI where wheels are published; Linux currently falls back to source distribution. |
| npm (Node) | `npm i -g plasmite` | Yes | Yes (Node bindings) | `official` (macOS x64/arm64; Linux/Windows x64) | Requires Node >=24; bundles addon, native assets, and CLI. |
| Go module | `go get github.com/sandover/plasmite/bindings/go/local` | No | Yes (Go bindings) | `official` (macOS/Linux) | Requires system SDK (brew/manual) for cgo; import pure contracts from `/api` when needed. |
| GitHub release tarball | Download from releases | Yes | Yes (SDK layout) | `official` (manual path) | Contains `bin/`, `lib/`, `include/`, `lib/pkgconfig/`. |
| GitHub Actions artifact (Linux ARM64) | Download `ci-sdk-aarch64-unknown-linux-gnu` | Yes | Yes (SDK layout) | `preview` | CI archive, retained for 30 days; includes checksum and build provenance. |
| GitHub Actions artifact (Linux ARMv7) | Download `ci-sdk-armv7-unknown-linux-gnueabihf` | Yes | Yes (SDK layout) | `preview` | 32-bit hard-float CI archive, retained for 30 days; includes checksum and build provenance. |
| cargo-binstall (Rust binary installer) | `cargo binstall plasmite --no-confirm` | Yes | No | `preview` (`x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`) | Uses `package.metadata.binstall` URL mapping to GitHub release SDK tarballs; release-publish smoke gate is currently Linux-only. |

**Requirements:** Node.js 24 or newer for the npm package and its CLI;
Python 3.10 or newer for the Python package; Go 1.22 or newer, cgo, and
pkg-config for Go. Rust source installs need the toolchain and C compiler
described in the [build guide](../building.md#install-the-cli-from-source).

Python's Linux source package needs a separate system SDK. For a manually
extracted SDK, point `PLASMITE_LIB_DIR` at its `lib` directory and add its
`bin` directory to PATH before running Python or its console command. A
Linux ARM SDK supplies the CLI and C library, but no npm native addon.

For changes from earlier versions, see the [1.0 upgrade guide](upgrading-1.0.md).
The secure access-key feature is in the 1.0 candidate; installing the currently
published 0.8 package does not add it.

The `GitHub release tarball` row applies to official macOS and Linux x86_64
archives. The Linux ARM archives are a separate GitHub SDK preview and do not
change npm, PyPI, or Homebrew support.

## Linux ARM SDK preview install

For a CI-built SDK, download the matching artifact from the
[ARM SDK CI run](https://github.com/sandover/plasmite/actions/runs/36803923793).
With GitHub CLI, download the ARM64 artifact using
`gh run download 36803923793 --repo sandover/plasmite -n ci-sdk-aarch64-unknown-linux-gnu`, or download
the ARMv7 artifact with:

```bash
gh run download 36803923793 --repo sandover/plasmite -n ci-sdk-armv7-unknown-linux-gnueabihf
```

Each artifact contains the SDK tarball, checksum, and build provenance. The same tarball names are planned for GitHub releases, but no release publishes them yet. See the
[build guide](../building.md#install-a-linux-arm-preview) for checksum, extraction,
and `PATH` commands. Each archive contains `plasmite` and `pls`, so Rust is not
required on the target machine.

Choose the archive from the operating system's userspace architecture. Run
`getconf LONG_BIT` on the Pi to distinguish 32-bit and 64-bit Linux; the kernel
architecture alone does not establish which userspace archive to use. Both
targets require glibc 2.35 or newer; check with `getconf GNU_LIBC_VERSION`. The
intended 32-bit target is a Raspberry Pi 2 running 32-bit Raspberry Pi OS Lite.
For a server, run the executable as the operating-system account that owns the
pool directory. Follow the [serving guide](serving.md#share-your-first-pool)
for directory setup and server operation. On ARMv7, pool files can be at most
2,147,483,647 bytes (2 GiB minus one byte); Plasmite rejects creation of a
larger pool or mapping of a larger existing pool. Smaller pools can still fail
to map when system memory is insufficient.

## cargo-binstall Promotion Criteria

Current tier is `preview`.

Promotion to `official` requires all of:
- Fail-closed release automation smoke coverage for every intended official target triple (not only Linux).
- Post-release delivery verification evidence for the same target set.
- Tier update in this document and corresponding release policy updates in `docs/record/releasing.md`.

## SDK Layout (Release Artifacts)

GitHub release tarballs are the single source of truth for the SDK layout:

```text
bin/
  plasmite
  pls
include/
  plasmite.h
lib/
  libplasmite.(dylib|so)
  pkgconfig/
    plasmite.pc
```

### pkg-config Contract

The `plasmite.pc` file must:
- Be named `plasmite` (not `libplasmite`)
- Provide `Cflags: -I...` for `include/plasmite.h`
- Provide `Libs: -L... -lplasmite`
- For Linux SDK artifacts, provide `Libs.private: -lpthread -ldl -lm` so `pkg-config --static --libs plasmite` resolves required system libraries.
