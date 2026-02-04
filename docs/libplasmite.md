<!--
Purpose: Document how to build and link against libplasmite.
Exports: N/A (documentation).
Role: Guide for bindings and consumers of the C ABI.
Invariants: Paths and artifact names match Cargo outputs.
Notes: This document is non-normative; ABI details live in include/plasmite.h.
-->

# libplasmite Build & Link Guide

`libplasmite` is the C ABI library used by official bindings.

## Build artifacts

Build debug artifacts:

```bash
just abi
```

Build release artifacts:

```bash
just abi-release
```

Artifacts are produced under:

- `target/debug/` or `target/release/`
- Typical names:
  - `libplasmite.dylib` (macOS)
  - `libplasmite.so` (Linux)
  - `libplasmite.a` (static)

## Header

The public header lives at:

- `include/plasmite.h`

## Linking

### Dynamic linking (recommended)

Link against `libplasmite.dylib` (macOS) or `libplasmite.so` (Linux) and ensure it is discoverable:

- set `LD_LIBRARY_PATH` (Linux) or `DYLD_LIBRARY_PATH` (macOS)
- or place the library in a standard system path

### Static linking

Link against `libplasmite.a` if your toolchain prefers static libraries.

## Overrides

Bindings may allow an override path via `PLASMITE_LIB_DIR` to point to a custom build output.

## Notes

- Remote pool refs are not supported in v0.
- JSON message envelopes must follow the CLI contract in `spec/v0/SPEC.md`.
