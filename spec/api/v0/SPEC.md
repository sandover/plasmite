# Plasmite Public API Spec v0

This document defines the normative public API contract for v0 across bindings.
It is intentionally signature-free: language-level method/function signatures live in code-level API docs.

## Scope

- This spec freezes cross-language semantics and invariants.
- This spec does not freeze binding-specific naming, argument ordering, or doc examples.

## Versioning + Compatibility

- The API surface is versioned as `v0`.
- Compatibility within v0 is additive-only.
- Existing field/operation meanings must not be removed or redefined.
- New fields must be optional with defaults that preserve old behavior.
- New operations are allowed if existing semantics remain stable.
- Any breaking change requires a new major API version.

## Stable Surface

### PoolRef

- `name("chat")`: resolves within the configured pool directory.
- `path("/abs/path/to/pool.plasmite")`: direct local path.
- `uri("tcp://host:port/pool/chat")`: accepted for forward compatibility.
- Local v0 clients reject URI refs with `Usage` (remote pool refs are not yet supported through local clients).

### Client + Pool Capabilities

- Clients expose pool lifecycle operations: create, open, info, list, delete.
- Pool handles expose message operations: append, get, tail.
- `list_pools` is scoped to the configured local pool directory.

## Data + Error Contract

### Core Data Types

- `Message` envelope semantics match `spec/v0/SPEC.md` (`seq`, `time`, `meta`, `data`).
- `PoolInfo` includes canonical local `path` and capacity/bounds diagnostics.
- `PoolInfo` fields are additive-only within v0.

### Error Kind Contract

Errors must carry a stable `kind` plus structured context when available (for example `path`, `seq`, `offset`).
Bindings must preserve kinds and expose context idiomatically.

Stable v0 kinds:

- `Usage`
- `NotFound`
- `AlreadyExists`
- `Busy`
- `Permission`
- `Corrupt`
- `Io`
- `Internal`

## Behavioral Semantics

### Required Operation Semantics

- `create_pool` creates a new pool and returns `AlreadyExists` if one already exists.
- Local create paths must create parent directories as needed (equivalent to `mkdir -p`).
- `open_pool` returns `NotFound` when target is missing.
- `delete_pool` may return `Busy` when the pool cannot be removed safely.
- `append` is atomic with respect to pool ordering and returns the committed envelope.
- `get` returns `NotFound` when `seq` is absent/out of range.
- `tail` preserves pool ordering by `seq`.

### Streaming Semantics

- Ordering is strictly by `seq` within a pool.
- Streams support explicit caller cancellation.
- Implementations must respect backpressure and avoid unbounded buffering.
- Once cancellation is observed, no further messages may be delivered.
- Reconnect behavior (for remote transports) must be explicit and must not reorder messages.

### Conformance

A binding is conformant when it implements the operation families above and preserves error kinds/semantics.
Conformance suites may rely on CLI spec formatting rules for shared message validation behavior.

## Non-Contract Surface

- Binding-specific naming, argument ordering, and exact method/function signatures.
- Binding-specific prose examples and convenience helpers.

## References

- CLI contract: `spec/v0/SPEC.md`
- Remote protocol contract: `spec/remote/v0/SPEC.md`
