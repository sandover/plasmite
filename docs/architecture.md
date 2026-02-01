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

Key invariants:
- Every committed message has a monotonically increasing `seq`.
- Every committed message has an RFC 3339 `time` (UTC).
- Corruption is detected and reported as an “invalid/corrupt pool” error kind.

## Preparing for remote access (TCP now, QUIC later)

Plasmite’s goal is to remain **transport-agnostic**:
- Standardize a `PoolRef` model that can represent local names/paths now and URI-based refs later.
- Define one framing format for message streams so stdin/file/TCP/QUIC can share the same logical events.
- Make `plasmite serve` (future) a thin adapter: socket ↔ framing ↔ core operations.

“UDP access” is expected to be delivered via **QUIC** (UDP-based transport with streams, reliability, and TLS), not bespoke unreliable UDP.

