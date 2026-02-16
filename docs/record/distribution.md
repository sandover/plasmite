# Distribution (v0.1.x)

What users get from each install channel, supported platforms, and the stable on-disk SDK layout.

## Support Tiers

A platform/channel combination is **supported** only when all of these are true:
- Users can install via an idiomatic channel command (e.g. `brew install`, `npm i -g`, `uv tool install`) without building from source.
- The install path is exercised by automated smoke checks in CI/release workflows.
- The combination is explicitly marked `official` below.

Official platforms:
- macOS: `aarch64-apple-darwin`, `x86_64-apple-darwin`
- Linux: `x86_64-unknown-linux-gnu`
- Windows: `x86_64-pc-windows-msvc` via npm and PyPI release artifacts

Not currently targeted:
- `aarch64-unknown-linux-gnu`
- Linux distro packages (`apt`, `yum`, `pacman`, etc.)
- `cargo-binstall` / other binary installer channels

## Install Matrix

| Channel | Install Command | Provides CLI | Provides Library | Tier | Notes |
| --- | --- | --- | --- | --- | --- |
| Homebrew (macOS and Linux) | `brew install sandover/tap/plasmite` | Yes | Yes (system SDK) | `official` | Installs `bin/`, `lib/`, `include/`, `pkg-config` metadata. |
| crates.io (Rust) | `cargo install plasmite` | Yes | No | `official` | Installs binaries into Cargo bin dir; source build. |
| crates.io (Rust) | `cargo add plasmite` | No | Yes (Rust crate) | `official` | Standard Rust dependency. |
| PyPI (Python) | `uv tool install plasmite` | Yes | Yes (Python bindings) | `official` (macOS/Linux/Windows x86_64) | Wheel bundles native assets and CLI. |
| npm (Node) | `npm i -g plasmite` | Yes | Yes (Node bindings) | `official` (macOS/Linux/Windows x86_64) | Bundles addon, native assets, and CLI. |
| Go module | `go get github.com/sandover/plasmite/bindings/go/plasmite` | No | Yes (Go bindings) | `official` (macOS/Linux) | Requires system SDK (brew/manual) for cgo. |
| GitHub release tarball | Download from releases | Yes | Yes (SDK layout) | `official` (manual path) | Contains `bin/`, `lib/`, `include/`, `lib/pkgconfig/`. |

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
