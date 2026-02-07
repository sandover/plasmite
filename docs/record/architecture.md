<!--
Purpose: Define Plasmite's core architecture so contributors can change internals without breaking user contracts.
Exports: N/A (documentation).
Role: Keystone technical reference for system boundaries, data flow, and invariants.
Invariants: Specs define external contracts; this doc explains implementation structure that must preserve those contracts.
-->

# Architecture

This is the implementation architecture for Plasmite.
Normative behavior lives in:

- `spec/v0/SPEC.md` (CLI contract)
- `spec/api/v0/SPEC.md` (public API)
- `spec/remote/v0/SPEC.md` (HTTP protocol)

## Design principles

- Local-first: pool storage correctness does not depend on external services.
- Contract-first: CLI/API/protocol stability is preserved while internals evolve.
- Shared core: CLI, bindings, and server reuse the same pool/message logic.
- Explicit failure modes: errors map to stable kinds and scriptable exit codes.

## Layer model

1. Interface layer
- CLI (`src/main.rs`), API (`src/api/*`), and HTTP server (`src/serve.rs`).
- Responsibilities: parse/validate inputs, invoke core operations, map results to user-facing formats.

2. Core domain layer
- Pool format and operations (`src/core/*`).
- Responsibilities: append/get/tail semantics, validation, sequencing, corruption detection.

3. Platform layer
- File mapping, file locking, and notify primitives.
- Responsibilities: concurrency and durability primitives across supported OSes.

Rule: interface layers do not implement storage correctness logic themselves.

## Data model and on-disk layout

A pool file is:

`header | index_region | ring`

- Header: metadata, bounds, and offsets.
- Index region: optional fixed-size seq->offset slots (`(u64 seq, u64 offset)`).
- Ring: append log frames containing encoded `{meta, data}` payloads.

Key invariants:

- Sequence numbers are monotonically increasing for committed messages.
- Frame commit state is validated before exposure to readers.
- Corrupt/torn/stale reads do not silently return invalid payloads.
- Index mismatches always fall back to scan for correctness.

## Write/read paths

Append (high level):

1. Validate input and prepare frame.
2. Acquire writer lock.
3. Plan placement in ring (including overwrite/wrap decisions).
4. Commit frame bytes.
5. Update index slot (when enabled).
6. Publish header updates and notify waiters.

Get-by-seq path:

1. Validate requested seq is in visible bounds.
2. Probe index slot.
3. If probe fails validation, fall back to scan.
4. Return decoded message envelope.

Tail path:

1. Resolve start position (`tail`, `since`, `from`).
2. Stream committed messages in order.
3. Use notify + bounded polling fallback for low-latency follow mode.

## Transport architecture

Plasmite is transport-agnostic at the core.

- Local mode: CLI/API calls directly into core operations.
- Remote mode: `plasmite serve` adapts HTTP request/response into the same core calls.
- Future transports (for example QUIC) should be adapters, not alternate correctness engines.

## Extension seams

- New interfaces should reuse `PoolRef` and core message operations.
- New payload conventions should preserve existing envelope semantics (`seq`, `time`, `meta`, `data`).
- Performance work must preserve crash-safety and fallback correctness paths.

## Operational guarantees vs non-goals

Guaranteed:

- Stable error-kind surface and exit-code mapping.
- Durable pool file invariants under normal crash and restart scenarios.
- Cross-surface behavior parity (CLI/API/HTTP) for the same operation class.

Not guaranteed:

- Hard real-time latency.
- Infinite retention (ring buffers are bounded).
- Backward compatibility for undocumented internal file details outside versioned formats.
