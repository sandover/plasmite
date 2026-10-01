# Plasmite Public API Spec (1.x)

This document defines the normative public API contract for Plasmite 1.x across bindings.
The historical path is retained; Rust source-level breaking changes are
documented in [the upgrade guide](../../../docs/record/upgrading-1.0.md).
It is intentionally signature-free: language-level method/function signatures live in code-level API docs.

## Scope

- This spec freezes cross-language semantics and invariants.
- This spec does not freeze binding-specific naming, argument ordering, or doc examples.
- Secure native connections and access-key operations below describe the Rust
  API used by the CLI. Other bindings retain their documented local APIs;
  Node's HTTP client does not load saved native connections or implement
  access-key certificate pinning.

## Versioning + Compatibility

- The API surface follows the package major version: `1.x`.
- Compatibility within 1.x is additive-only.
- Existing field/operation meanings must not be removed or redefined.
- New fields must be optional with defaults that preserve old behavior.
- New operations are allowed if existing semantics remain stable.
- Any breaking change requires a new major API version. The secure-sharing
  release is a breaking major-version change and
  removes superseded secure connection options.

## Stable Surface

### PoolRef

- `name("chat")`: resolves within the configured pool directory.
- `path("/abs/path/to/pool.plasmite")`: direct local path.
- `uri("https://host:port/<pool>")`: remote pool URL using the existing CLI
  shorthand form. A connected client uses the saved credentials for that
  destination.

### Client + Pool Capabilities

- Rust clients expose pool lifecycle operations: create, open, info, list, delete.
  Other bindings expose their documented subsets; shared semantics apply to
  operations they implement. Conformance adapters may use CLI inspection.
- Pool handles expose message operations: append, get, tail.
- `list_pools` is scoped to the configured local pool directory.
- Rust exposes native `connect(server_url, access_key)` and
  `status(server_url)` and `disconnect(server_url)` operations. `connect`
  verifies the HTTPS server before saving the connection for the current OS
  user; a failed verification or save leaves any prior connection intact.
  `status` reports whether credentials are saved, the destination is reachable,
  and the server accepts the saved secret without exposing credentials.
  `disconnect` removes the local saved credential without contacting the
  server. It succeeds when the destination is unavailable, is safe to repeat,
  and does not revoke the server's key or affect other destinations.
- Several saved connections may coexist. Create a `RemoteClient` for each
  destination; it reloads that destination's saved credential and matching TLS
  pin before each request. An existing client observes credential replacement
  and disconnect. A client created with `with_access_key` keeps its explicit
  key. A pool URI used with the client must name the same origin. Remote
  shorthand URLs keep the form `https://host:port/<pool>`.
- Saved credentials are selected by the explicitly configured HTTPS origin.
  They do not move when an address changes. The user can explicitly connect to
  a new address with the same key only after that address presents the pinned
  public key and accepts the secret.
- An access key has the form `pk1.<spki-fingerprint>.<secret>`. The
  fingerprint is the 64-character lowercase hexadecimal SHA-256 digest of
  the certificate's DER Subject Public Key Info (SPKI); the secret is 32
  random bytes encoded as 64 lowercase hexadecimal characters. The
  destination address is separate from the key.
- Before sending the secret, a Rust native remote client verifies the destination hostname,
  certificate validity, TLS proof of possession, and the public-key
  fingerprint embedded in the key. It never forwards saved credentials to a
  different destination after a redirect.

## Data + Error Contract

### Core Data Types

- `Message` envelope semantics match `spec/v0/SPEC.md` (`seq`, `time`, `meta`, `data`).
- `PoolInfo` includes canonical local `path` and capacity/bounds diagnostics.
- `PoolInfo` fields are additive-only within 1.x.

### Error Kind Contract

Errors must carry a stable `kind` plus structured context when available (for example `path`, `seq`, `offset`).
Bindings must preserve kinds and expose context idiomatically.

Stable error kinds:

- `Usage`
- `NotFound`
- `AlreadyExists`
- `Busy`
- `Permission`
- `Corrupt`
- `Io`
- `Internal`
- `RetentionGap`

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
- A local tail cancellation flag ends the tail promptly and returns no further
  messages after cancellation is observed.
- Reconnect behavior (for remote transports) must be explicit and must not reorder messages.
- Remote one-shot operations end with `Io` when the server sends no complete
  response within a deadline (30 seconds in the Rust client). Remote streams
  have no deadline, because a stream on a quiet pool carries nothing.

Pools use bounded retention, so a stream cursor can be overtaken when writers
wrap the ring buffer. Every tail tracks an expected sequence:

- An explicit `since_seq` is inclusive and establishes that value as the first
  expected sequence.
- Without `since_seq`, the first available raw message establishes the
  position. Missing history before that message is not reported as a gap.
- Each raw message advances the expected sequence before tag or payload
  filtering. A filter therefore cannot conceal a retention gap.
- A raw sequence greater than the expected sequence is a retention gap. A raw
  sequence below it is skipped as already observed.
- The default policy continues from the next retained message. This preserves
  the behavior of existing callers but does not guarantee complete delivery.
- The optional fail-closed policy returns `RetentionGap` before delivering the
  first message after a gap. The error's `seq` is the first missing sequence,
  and the affected stream then terminates.

After `RetentionGap`, the consumer decides how to recover: select a new
checkpoint, rebuild derived state from another source, or accept the loss and
restart. Plasmite does not acknowledge consumption or prevent overwrite.

The local decoded and Lite3 SDK tails support both policies. Remote JSON tails
support both policies through `gap_policy=continue|error`. Remote Lite3 tails
support continuation only because their frame format has no terminal error
representation.

CLI drop notices and MCP `fell_behind` fields remain diagnostics for their
existing cursor models. They express the same bounded-retention condition but
do not change into SDK `RetentionGap` errors.

### Conformance

A binding is conformant when it implements the operation families above and preserves error kinds/semantics.
Conformance suites may rely on CLI spec formatting rules for shared message validation behavior.

### Replay

Rust replay captures its initial retained-history boundary; later appends do
not prolong construction. A requested tail retains at most that many matches
after the API's time filter. Replay speed must be finite and nonnegative;
zero delivers immediately. Unrepresentable playback delays return `Usage`.
This API eagerly owns its selected messages; CLI replay separately streams
bounded cursor state.

## Non-Contract Surface

- Binding-specific naming, argument ordering, and exact method/function signatures.
- Binding-specific prose examples and convenience helpers.

## References

- CLI contract: `spec/v0/SPEC.md`
- Remote protocol contract: `spec/remote/v0/SPEC.md`
