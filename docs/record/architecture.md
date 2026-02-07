<!--
Purpose: Describe Plasmite’s internal boundaries so contributors can extend it without coupling UX, storage, and transport concerns.
Exports: N/A (documentation).
Role: Architectural guide (non-normative) that complements the spec and ADRs.
Invariants: The CLI contract remains transport-agnostic; storage integrity errors are surfaced consistently.
-->

# Architecture

This document explains how Plasmite is structured and where responsibilities live.
It is **not** a CLI contract; the normative contract for v0 lives in `spec/v0/SPEC.md`.

## Layers

### Contract surface (CLI)

The `plasmite` binary is JSON-first and Unix-friendly:
- Reads JSON from args / stdin.
- Writes JSON to stdout (JSONL for streams).
- Writes human-readable errors to stderr on TTY; JSON errors on non-TTY stderr.

This layer should not “know” about networking beyond resolving a pool reference and invoking core operations.

### Core operations (pool + messages)

Core logic owns:
- Pool file layout and invariants (append-only log with bounded retention).
- Locking and concurrency semantics.
- Encoding/decoding the stored payload.
- Stable error kinds (mapped to stable exit codes in the CLI).

Core should be callable from:
- CLI commands (local use).
- Future servers (remote access) without re-implementing correctness rules.

### Storage format (on disk)

At the CLI boundary messages are JSON; on disk the payload is a compact binary encoding of `{meta,data}`.
Pool files use `header | index region | ring` layout; the inline index provides fast seq->offset lookup with scan fallback for stale/collided slots.

Key invariants:
- Every committed message has a monotonically increasing `seq`.
- Every committed message has an RFC 3339 `time` (UTC).
- Corruption is detected and reported as an “invalid/corrupt pool” error kind.

## Remote access

Plasmite is **transport-agnostic**:
- The `PoolRef` model represents local names/paths; remote shorthand URLs are supported for `poke` (and more commands soon).
- Message streams use the same logical format whether over stdin, file, HTTP, or future transports.
- `plasmite serve` is a thin adapter: HTTP ↔ framing ↔ core operations.

### Current: HTTP/JSON (v0)

`plasmite serve` exposes pools over HTTP with JSON request/response bodies:
- Loopback by default in v0; non-loopback binds require explicit opt-in
- See `spec/remote/v0/SPEC.md` for the protocol contract
- Node.js `RemoteClient` provides a typed client

### Future: QUIC transport

"UDP access" will be delivered via **QUIC** (UDP-based transport with streams, reliability, and TLS), not bespoke unreliable UDP. This will enable lower-latency streaming and better handling of unreliable networks.
