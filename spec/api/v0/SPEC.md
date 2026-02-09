# Plasmite Public API Spec v0

This document is the **normative** contract for the Plasmite public API v0.
The CLI contract remains normative for scripting behavior in `spec/v0/SPEC.md`.

## Versioning + Compatibility

- The API is versioned as `v0`.
- **Additive-only within v0**:
  - new fields must be optional with defaults that preserve old behavior,
  - new operations may be added without changing existing semantics,
  - existing fields/operations **must not** change meaning or remove behaviors.
- Any breaking change requires a new major API version.

## Core Types

### PoolRef

Identifies a pool without fixing transport.

- `name("chat")`: resolved within the configured pool directory.
- `path("/abs/path/to/pool.plasmite")`: direct path to a pool.
- `uri("tcp://host:port/pool/chat")`: reserved for future remote pools.

### Client

A transport-aware resolver that produces pools and pool metadata.

- `create_pool(ref, options) -> PoolInfo`
- `open_pool(ref, options) -> Pool`
- `pool_info(ref) -> PoolInfo`
- `list_pools(dir) -> [PoolInfo]` (local-only)
- `delete_pool(ref) -> ()`

### Pool

A handle to a specific pool.

- `append(pool, AppendRequest) -> Message`
- `get(pool, seq) -> Message`
- `tail(pool, TailRequest) -> Stream<Message>`

### Message

Message envelope shared with the CLI schema.

- `seq: u64` (monotonic per pool)
- `time: RFC3339 UTC string`
- `meta: Meta`
- `data: JSON value`

### Meta

- `tags: []string`
- Future fields must be additive and namespace-aware.

### PoolInfo

- `uuid: string`
- `path: string`
- `size_bytes: u64`
- `created_at: RFC3339 UTC string`
- `updated_at: RFC3339 UTC string`

## Operation Semantics

### create_pool

- Creates a new pool; fails with `AlreadyExists` if the pool exists.
- Options may include size, retention, or durability defaults (additive).

### open_pool

- Opens an existing pool; fails with `NotFound` if missing.
- Options may include read-only or validation flags (additive).

### pool_info

- Returns metadata for the pool at `ref`.

### list_pools

- Returns all pools within a directory; errors if the directory is invalid.

### delete_pool

- Removes the pool; a busy pool may return `Busy`.

### append

- Appends a message and returns the committed envelope.
- Must be atomic with respect to the pool ordering.
- `meta.tags` are stored verbatim; ordering is preserved.

### get

- Returns a message by `seq`.
- `NotFound` if the `seq` is out of range or absent.

### tail

- Returns a stream of messages starting at a requested cursor or time window.
- The stream must preserve the pool ordering.

## Error Kinds

Errors must carry a stable **kind** plus structured context (path, pool ref, seq, offset when applicable).
Bindings must preserve the kind and expose the context idiomatically.

Stable kinds (v0):

- `Usage`
- `NotFound`
- `AlreadyExists`
- `Busy`
- `Permission`
- `Corrupt`
- `Io`
- `Internal`

## Streaming Semantics

- Ordering is strictly by `seq` within a pool.
- Streams must support explicit cancellation by the caller.
- Backpressure must be respected; implementations must not unboundedly buffer messages.
- If a stream is canceled, no further messages may be delivered after cancellation is observed.
- For remote transports, reconnection semantics must not reorder messages and must be explicit.

## Conformance Expectations

- A binding is conformant if it implements all core types and operations and preserves error kinds.
- Conformance tests may use the CLI spec for message formatting details and validation rules.
