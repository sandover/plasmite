<!--
Purpose: Provide a single entry point to Plasmite documentation and keep the doc set discoverable as it grows.
Exports: N/A (documentation).
Role: Docs index (non-normative); links to the spec and supporting docs.
Invariants: Normative contracts live under spec/; this file must not restate versioned guarantees.
-->

# Docs

## Start here

- Vision: `docs/vision.md`
- Architecture: `docs/architecture.md`
- Roadmap: `docs/roadmap.md`
- Decisions (ADRs): `docs/decisions/README.md`

## Normative specs

- v0 CLI + message contract: `spec/v0/SPEC.md`
- v0 public API contract: `spec/api/v0/SPEC.md`
- v0 remote protocol: `spec/remote/v0/SPEC.md`

## Quickstart guides

- CLI usage: see the [root README](../README.md)
- Rust API: `docs/api-quickstart.md`
- Go bindings: `docs/go-quickstart.md`
- Python bindings: `bindings/python/README.md`
- Node.js bindings: `bindings/node/README.md`

## Language bindings

Official bindings for embedding Plasmite without the CLI:

| Language | Location | Notes |
|----------|----------|-------|
| Go | `bindings/go/` | cgo wrapper over libplasmite |
| Python | `bindings/python/` | ctypes wrapper over libplasmite |
| Node.js | `bindings/node/` | N-API wrapper; includes `RemoteClient` |

All bindings use the C ABI (`libplasmite`). See `docs/libplasmite.md` for build/link instructions.

## Remote access

Plasmite includes an HTTP server (`plasmite serve`) for remote pool access:

- Protocol spec: `spec/remote/v0/SPEC.md`
- Node.js `RemoteClient`: `bindings/node/README.md`
- Deployment + security notes: see “Remote Access” in `README.md`

## CLI reference

- Exit codes: `docs/exit-codes.md`
- Diagnostics (`doctor`): `docs/doctor.md`

## Development

- Testing: `docs/TESTING.md`
- Releasing: `docs/releasing.md`
- Homebrew packaging: `docs/homebrew.md`
- Performance baselines: `docs/perf.md`
- Conformance suite: `conformance/README.md`

## C ABI (libplasmite)

- Build and link guide: `docs/libplasmite.md`
- Header: `include/plasmite.h`

## Design notes

- Storage + concurrency TDD: `docs/design/tdd-v0.0.1.md`
- Public API TDD (proposal): `docs/design/tdd-public-api-v0.md`
- Correctness refactor notes: `docs/design/pool-correctness-refactor.md`
- Snapshot notes: `docs/design/pool-snapshots.md`
