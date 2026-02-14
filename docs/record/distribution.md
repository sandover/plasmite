# Distribution Contract (v0.1.x)

This document defines:
- What users get from each install channel (CLI and/or language bindings)
- The stable on-disk layout for release artifacts ("the SDK layout")
- Supported platforms and explicit non-goals

## Supported Platforms

- macOS: `aarch64-apple-darwin`, `x86_64-apple-darwin`
- Linux: `x86_64-unknown-linux-gnu`
- Windows preview (best-effort): `x86_64-pc-windows-msvc` via GitHub release preview zip assets

Non-goals for now:
- Windows as a release-gating, fully supported channel
- `aarch64-unknown-linux-gnu`
- Linux distro packages (`apt`, `yum`, `pacman`, etc.)
- `cargo-binstall` / other binary installer channels

## Install Matrix

| Channel | Install Command | Provides CLI | Provides Library | Notes |
| --- | --- | --- | --- | --- |
| Homebrew (macOS and Linux) | `brew install sandover/tap/plasmite` | Yes | Yes (system SDK) | Installs `bin/`, `lib/`, `include/`, `pkg-config` metadata. |
| crates.io (Rust) | `cargo install plasmite` | Yes | No | Installs binaries into Cargo bin dir. |
| crates.io (Rust) | `cargo add plasmite` | No | Yes (Rust crate) | Standard Rust dependency. |
| PyPI (Python) | `uv tool install plasmite` | Yes | Yes (Python bindings) | Wheel bundles native assets and CLI on supported targets. |
| npm (Node) | `npm i -g plasmite` | Yes | Yes (Node bindings) | Bundles addon, native assets, and CLI on supported targets. |
| Go module | `go get github.com/sandover/plasmite/bindings/go/plasmite` | No | Yes (Go bindings) | Requires system SDK installed (brew/manual) for cgo. |
| GitHub release tarball | Download from releases | Yes | Yes (SDK layout) | Contains `bin/`, `lib/`, `include/`, `lib/pkgconfig/`. |
| GitHub Windows preview zip | Download `plasmite_<version>_windows_amd64_preview.zip` from releases | Yes | Partial SDK (`bin/`, `lib/`, `include/`) | Best-effort preview only; paired `.sha256` sidecar is published with each asset. |

## Windows Preview Policy

- Distribution mechanism: manual preview workflow (`.github/workflows/windows-preview.yml`) uploads Windows preview zip assets to an existing release tag.
- Scope: this does not alter `release-publish.yml` or official release-gating channels.
- Support level: best-effort preview for user convenience while runtime/CI hardening continues.
- Fallback guidance: if local Windows write paths fail, use remote-only flows against `plasmite serve` on Linux/macOS.

## SDK Layout (Release Artifacts)

GitHub release tarballs are the single source of truth for the SDK layout. The root directory contains:

```text
bin/
  plasmite
  pls
include/
  plasmite.h
lib/
  libplasmite.(dylib|so)
  libplasmite.a              (optional; see Decisions)
  pkgconfig/
    plasmite.pc
```

### pkg-config Contract

The `plasmite.pc` file must:
- Be named `plasmite` (not `libplasmite`)
- Provide `Cflags: -I...` for `include/plasmite.h`
- Provide `Libs: -L... -lplasmite`
- Provide any required runtime rpath guidance where feasible (see Loader Notes)

## Loader Notes (Hard Part)

The hardest part of a batteries-included SDK is dynamic loader correctness across:
- system installs (Homebrew in `/opt/homebrew` or `/usr/local`)
- bundled installs (Python wheels / npm packages with package-local native libs)

Targets:
- macOS: prefer a shared library identity that can be rewritten by packagers
  - recommended: dylib id `@rpath/libplasmite.dylib` for release artifacts
  - Homebrew formula may patch the id to an absolute path under `#{lib}`
- Linux: decide between a stable SONAME scheme or `$ORIGIN`-based rpaths for bundled installs

## Decisions (Consult-Me)

Decided:
- npm naming/structure: publish canonical `plasmite` on npm (migrate from `plasmite-node`)
- CLI bundling policy for Python/Node: bundle the Rust `plasmite` CLI binary in wheels/npm packages
- Version coupling: lockstep versions across cargo/PyPI/npm and GitHub tags

Open:
- Static lib shipping: shared-only for now (no `libplasmite.a` guarantee in release artifacts)
