<!--
Purpose: Document how to build and link against libplasmite.
Exports: N/A (documentation).
Role: Guide for bindings and consumers of the C ABI.
Invariants: Paths and artifact names match Cargo outputs.
Notes: This document is non-normative; ABI details live in include/plasmite.h.
-->

# libplasmite Build & Link Guide

`libplasmite` is the C ABI library used by official bindings.

## ABI stability policy

Within v0:
- additive changes only
- no breaking renames/removals
- conformance suite only grows

Breaking changes require a new major (v1), a migration guide, and a parallel
support window when feasible. The ABI contract is versioned independently (for
example, `libplasmite.so.0` where applicable).

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

## Ownership rules

- `plsm_client_t`, `plsm_pool_t`, and `plsm_stream_t` are opaque handles.
- Free handles with `plsm_client_free`, `plsm_pool_free`, and `plsm_stream_free`.
- Free buffers and errors with `plsm_buf_free` and `plsm_error_free`.
- All returned JSON payloads are owned by the caller until freed.

## Linking

### Dynamic linking (recommended)

Link against `libplasmite.dylib` (macOS) or `libplasmite.so` (Linux) and ensure it is discoverable:

- set `LD_LIBRARY_PATH` (Linux) or `DYLD_LIBRARY_PATH` (macOS)
- or place the library in a standard system path

### Static linking

Link against `libplasmite.a` if your toolchain prefers static libraries.

## Binding-specific notes

### C

- Include `include/plasmite.h`.
- Link with `-lplasmite` and an `-L` path to `target/debug` or `target/release`.

### Go

- The official module lives at `bindings/go`.
- `CGO_LDFLAGS` should point at `target/debug` or `target/release`.

### Python

- Load `libplasmite` with `ctypes`/`cffi` and use `include/plasmite.h` for the ABI.
- The official binding will live under `bindings/python`.

### Node/TypeScript

- Use `node-ffi`/`napi-rs` style bindings to load `libplasmite`.
- The official binding will live under `bindings/node`.

## Overrides

Bindings may allow an override path via `PLASMITE_LIB_DIR` to point to a custom build output.

## Notes

- Remote pool refs are not supported in v0.
- JSON message envelopes must follow the CLI contract in `spec/v0/SPEC.md`.
