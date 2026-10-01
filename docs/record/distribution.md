# Distribution (v0)

This document defines what users get from each install channel, supported platforms, and the stable on-disk SDK layout.

Native target identifiers and per-target channel tiers are owned by
`release/targets.json`. This document explains the support policy in human
terms and intentionally repeats important identifiers so readers do not need to
inspect build data. `scripts/validate_distribution_targets.sh` fails when those
lists drift apart.

## Support Tiers

A platform/channel combination is `official` only when all of these are true:
- Users can install via an idiomatic channel command (e.g. `brew install`, `npm i -g`, `uv tool install`) without building from source.
- The install path is exercised by automated smoke checks in CI/release workflows.
- The combination is explicitly marked `official` in the install matrix.

A combination can be `preview` when install metadata/artifacts are wired and at least one release-time smoke gate exists, but full target-matrix coverage is not yet complete.

Official platforms:
- macOS: `aarch64-apple-darwin`, `x86_64-apple-darwin`
- Linux: `x86_64-unknown-linux-gnu`
- Windows: `x86_64-pc-windows-msvc` via npm and PyPI release artifacts

Not currently targeted:
- Linux distro packages (`apt`, `yum`, `pacman`, etc.)

GitHub SDK preview platforms:
- Linux ARM64: `aarch64-unknown-linux-gnu` (`linux_arm64`)
- Linux ARMv7, 32-bit hard-float: `armv7-unknown-linux-gnueabihf` (`linux_armv7`)

These preview archives are configured to provide the CLI/server and SDK layout
for manual installation without Rust. This implementation does not publish
them. Once a release publishes them, the ARM64 archive will target Ubuntu 22.04
with glibc 2.35 as the intended runtime baseline. The ARMv7 archive will target
Armv7-A, VFPv3-D16 hard-float, and Thumb-2, with NEON optional, using an Ubuntu 22.04
glibc 2.35 sysroot. Release smoke checks are configured, but physical
Raspberry Pi compatibility and published-release delivery remain pending.
ARMv6 is outside this preview.

## Install Matrix

| Channel | Install Command | Provides CLI | Provides Library | Tier | Notes |
| --- | --- | --- | --- | --- | --- |
| Homebrew (macOS and x86_64 Linux) | `brew install sandover/tap/plasmite` | Yes | Yes (system SDK) | `official` | Installs `bin/`, `lib/`, `include/`, `pkg-config` metadata; post-release macOS install smoke required. |
| crates.io (Rust) | `cargo install plasmite` | Yes | No | `official` | Installs binaries into Cargo bin dir; source build. |
| crates.io (Rust) | `cargo add plasmite` | No | Yes (Rust crate) | `official` | Standard Rust dependency. |
| PyPI (Python) | `uv tool install plasmite` | Yes | Yes (Python bindings) | `official` (macOS/Windows x86_64) | Wheel bundles native assets and CLI where wheels are published; Linux currently falls back to source distribution. |
| npm (Node) | `npm i -g plasmite` | Yes | Yes (Node bindings) | `official` (macOS/Linux/Windows x86_64) | Bundles addon, native assets, and CLI. |
| Go module | `go get github.com/sandover/plasmite/bindings/go/local` | No | Yes (Go bindings) | `official` (macOS/Linux) | Requires system SDK (brew/manual) for cgo; import pure contracts from `/api` when needed. |
| GitHub release tarball | Download from releases | Yes | Yes (SDK layout) | `official` (manual path) | Contains `bin/`, `lib/`, `include/`, `lib/pkgconfig/`. |
| cargo-binstall (Rust binary installer) | `cargo binstall plasmite --no-confirm` | Yes | No | `preview` (`x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`) | Uses `package.metadata.binstall` URL mapping to GitHub release SDK tarballs; release-publish smoke gate is currently Linux-only. |

The `GitHub release tarball` row applies to official macOS and Linux x86_64
archives. The Linux ARM archives are a separate GitHub SDK preview and do not
change npm, PyPI, or Homebrew support.

## Linux ARM SDK preview install

Once a release publishes the preview assets, download
`plasmite_<version>_linux_arm64.tar.gz` for 64-bit ARM Linux or
`plasmite_<version>_linux_armv7.tar.gz` for 32-bit ARM Linux with the hard-float
ABI. This implementation configures the archives but does not publish them.
Extract a published archive and add its `bin/` directory to `PATH`; it contains
`plasmite` and `pls`, so Rust is not required on the target machine.
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
