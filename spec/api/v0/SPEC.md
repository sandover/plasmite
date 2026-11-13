# Plasmite Public API Spec (source checkout)

This document defines the public API contract across bindings. The source checkout
includes breaking changes that await the next major release; see the
[next major upgrade guide](../../../docs/record/upgrading-next-major.md).
Published 1.x APIs retain their release-specific contract.
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

- The API surface follows the package major version. Do not ship the pending
  SDK replay removals or UDP behavior changes under a 1.x package version.
- Compatibility within 1.x is additive-only.
- Existing field/operation meanings must not be removed or redefined.
- New fields must be optional with defaults that preserve old behavior.
- New operations are allowed if existing semantics remain stable.
- Any breaking change requires a new major API version. The secure-sharing
  release is a breaking major-version change and
  removes superseded secure connection options.

## Owned-view interface (additive implementation target)

This section defines the interface under development in epic `55CLVD`. Existing
entry points keep their documented behavior. Bindings introduce explicit view
entry points during evaluation; the next major release can make views primary.
Implementation status and actual spellings belong in each binding's API docs.

### Ownership and requested work

Append, get and tail return immutable message views with exact sequence and
nanosecond identity, tags and object-root data. Native strings never implicitly
mean JSON source. JSON input uses an explicit input boundary. An append view
retains the encoding that committed; forwarding with unchanged tags reuses the
payload and receives fresh destination identity. Changing tags/subtrees rebuilds
only the required encoding. An I/O error during flush can follow publication;
callers must not infer rollback or retry automatically.

A message owns a stable, four-byte-aligned Lite3 snapshot. Children share that
owner and remain valid after pool/stream/root-wrapper close or ring overwrite.
The local read copies the selected frame under the storage lock, then releases
the lock before caller code runs. Keeping a child keeps its complete snapshot.
C callers retain an owner through each borrowed-reference operation; independent
owner retain/release and immutable reads need no serialization with a pool.

Validate an acquired snapshot once from envelope offset zero, including metadata.
Count the root at depth zero and reject a container at depth 32. Check bounds,
alignment, strings, supported types and finite numbers. A checked private encoder
can establish that proof during construction. Mutable caller storage needs
isolation; a read-only wrapper alone does not prove immutable backing bytes.
Binary validation does not require JSON/base64 output.

Lookup, unique-key enumeration, length and conversion agree: duplicate encoded
keys use the last entry in native iteration order. JSON input uses the last
source occurrence. New writers reject NUL keys; NUL lookup means absent. Missing,
null and wrong type remain distinct. Strings preserve valid Unicode and embedded
NUL values; input conversion must not silently replace unpaired surrogates.

Explicit native conversion builds independent containers for only the requested
subtree; whole-message conversion produces `seq`, RFC3339 `time`, `meta.tags`
and `data`. JSON conversion formats directly from Lite3. Ordinary assignment,
child access and shallow copies retain child views. Mutable byte exports copy;
read-only byte borrows retain the owner. Time objects and raw JSON arise on
request rather than eagerly on arrival.

### Numbers and input hooks

New view writers accept exact signed 64-bit integers and finite binary64 values,
including negative zero. Exact integers outside i64 fail before publication,
including raw JSON integer tokens; decimal/exponent JSON tokens request binary64.
This new policy does not redefine the legacy unsigned-to-float conversion below.
Envelope sequence/time remain exact unsigned 64-bit identities.

JavaScript reads safe stored integers as number, larger stored integers as bigint,
and binary64 as number. Envelope identities always use bigint. Native safe
integral numbers use i64 except negative zero; other finite numbers use binary64.
A small input bigint can therefore read back as number. Integer arithmetic can
normalize with BigInt(value); JSON.stringify can throw on large stored integers.
Python uses int; Rust/C/Go use checked native conversions. Each direct encoder
preserves its declared hooks, getters, undefined/hole and custom-serializer
semantics, or rejects unsupported input explicitly. Buffer growth never reruns
user hooks. A generic TypeScript schema assertion supplies no runtime validation.

### Language protocols

| Binding | View and conversion idiom |
| --- | --- |
| Rust | Owned MessageView, borrowed ValueRef and typed decoding; borrowed strings/bytes stay tied to the owner. |
| C | Explicit owner retain/release, borrowed tagged values and sequential/coarse reads; no allocation per child handle. |
| Python | Mapping/Sequence indexing, iteration and readonly slices; to_native/to_json and an owning readonly buffer exporter. |
| JavaScript | Asynchronous storage operations; synchronous Proxy properties and actual arrays; module-level toNative/toJson helpers. |
| Go | Managed immutable bytes, blocking Get, explicit typed Decode and context-aware Tail; no Close per ordinary message. |

Python Mapping equality and array-view/list equality compare contents without
building temporary native trees. JavaScript array indices/length describe data;
Array prototype methods keep their normal behavior. Object payload names remain
data. Reflection returns unique keys; mutation and freeze/seal throw TypeError.
Live children keep stable identity through weak caching. Native materialization
defines own properties so __proto__ stays data. Spread is shallow; Object.assign
into an ordinary object follows setters. structuredClone does not accept Proxies.

Each transport keeps its existing delivery contract. In particular, HTTP binary
tails remain continue-only and cannot report terminal post-start errors; wrappers
reject unsupported strict-gap requests. UDP keeps arrival order, duplicates,
late packets and explicit skip/progress events without inferred repair.

Shared input cases live in `tests/fixtures/owned_views.json`. Run
`uv run python tests/owned_view_contract.py` for the input oracle. The standalone
`examples/owned_views.ts` checks target TypeScript usage without claiming that
those API declarations already ship. Runtime tasks consume these cases and add
lifetime, mutation, reflection and encoded-duplicate checks to their own suites.

### Additive Rust view operations

`PoolApiExt::append_value` serializes native data directly into a retained
`MessageView`; `append_view` forwards an existing immutable encoding and assigns
a fresh destination identity. `get_view` copies and validates one snapshot.
Existing message/JSON methods keep their published behavior.

`MessageView::encode` constructs an uncommitted encoding; `from_json` accepts
explicit JSON source with the strict integer rules above. Their sequence/time
start at zero until an append assigns them. `ValueRef::decode` builds a Serde
target directly and can borrow strings/bytes from its owner. Struct decoding
scans fields. `ValueRef::into_owned` returns an `OwnedValue` that explicitly
shares the complete snapshot; `as_ref` borrows it again. Serde serialization of a `ValueRef` preserves native byte values;
explicit `to_json` uses the canonical base64 byte representation. Re-encoding a
value ref with different tags constructs a new encoding without a native tree.

## Stable Surface

### PoolRef

- `name("chat")`: resolves within the configured pool directory.
- `path("/abs/path/to/pool.plasmite")`: direct local path.
- `uri("https://host:port/<pool>")`: remote pool URL using the existing CLI
  shorthand form. A connected client uses the saved credentials for that
  destination.

### Default local directories and string references

The default pool directory is the current user's `.plasmite/pools` directory.
Unix clients select an absolute, nonempty `HOME`, then the account home;
Windows clients select an absolute `USERPROFILE`, then the platform profile.
Failure to resolve an absolute home requires an explicit directory. Clients do
not silently choose the working directory or temporary storage. Explicit client
directories and pool paths take precedence and may be relative.

The C ABI and its string wrappers distinguish names from paths through native
path separators. A bare filename is a name; prefix it with `./` (or `.\` on
Windows) for a file in the working directory. Windows drive-relative paths are
unsupported. Go's `PoolRefPath` adds a relative prefix to bare filenames.

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

- Rust `Meta::from_lite3` reads metadata and validates the entire data payload
  without constructing its JSON value tree. A checked binary walk validates
  JSON-compatible values; native byte values retain the legacy base64 export
  check. Malformed metadata or unreadable data returns `Corrupt`.
- `Message` envelope semantics match `spec/v0/SPEC.md` (`seq`, `time`, `meta`, `data`).
- `PoolInfo` includes canonical local `path` and capacity/bounds diagnostics.
- `PoolInfo` fields are additive-only within 1.x.

### Stored numbers and JSON reads

Payload storage uses signed 64-bit integers and IEEE 754 binary64 floating-point
values. A JSON read formats the stored floating-point value so a correctly
rounded binary64 parser recovers the same bits, including negative zero. Rust
object reads use a parser configuration that preserves that round trip. An
intermediate decimal conversion must not introduce another rounding step.

The existing Lite3 encoder converts unsigned payload integers above
`i64::MAX` to binary64, which can lose integer precision. Exact unsigned 64-bit
sequence and timestamp identity belongs to the envelope/frame fields and keeps
its separate representation. This change preserves the storage format and range.

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
- `delete_pool` waits for an active append to finish before removing the pool.
  Deletion requires permission to open the target for locking. It may return
  `Busy` when the pool cannot be removed safely.
- An append through an open handle returns `NotFound` if its path no longer
  names that pool. Open the replacement pool before writing to it.
- `append` is atomic with respect to pool ordering and returns the committed envelope.
- Lite3 message appends validate the required message shape and readable nested
  data before writing. Invalid input returns `Corrupt` without advancing the pool.
  Valid appends preserve the input bytes and assign destination sequence/time.
- `PoolApiExt::append_json_identity` and `append_lite3_identity` return
  `(sequence, timestamp_ns)` from the successful append. Their `_now` variants
  generate unsigned Unix nanoseconds once and use that exact timestamp for both
  storage and result. They preserve input validation, durability and ordering.
  The local Pool implementation never rereads the ring or formats a timestamp
  string. Full-message append
  methods keep their existing snapshot semantics.
- The C identity variants write a caller-owned `plsm_append_identity_t` only on
  success. Python exposes `AppendReceipt` through `append_identity`,
  `append_json_identity` and `append_lite3_identity`. Node exposes
  `appendIdentity` and `appendLite3Identity`, with both fields as `bigint`.
  Go exposes `api.AppendReceipt` and optional backend helpers without adding
  required methods to the `api.Pool` interface. Unsupported Go backends fail
  before appending. UDP send provides no committed identity.
- `PoolApiExt::append_lite3_with_receipt` prepares the message from input before
  writing and returns its destination sequence/time without reading the pool
  again. `RemotePool::append_lite3_with_receipt` returns that message from the
  append response, without a second remote fetch. Existing Lite3 append methods
  keep returning the destination sequence number.
- `get` returns `NotFound` when `seq` is absent/out of range.
- `tail` preserves pool ordering by `seq`.

### Streaming Semantics

The C interface adds bounded waits without changing legacy `*_next` calls:

- `plsm_stream_next_wait` and `plsm_lite3_stream_next_wait` take a per-call
  wait budget in milliseconds. They return 1 for a message, 0 for wait expiry,
  2 for stream completion (including the original open-time deadline), 3 for
  cancellation, and -1 for an error. Wait expiry retains the cursor and counts.
  A zero budget polls once. A file operation already in progress may exceed the
  wait budget; the budget cannot interrupt an operating-system file lock.
- `plsm_stream_cancel` and `plsm_lite3_stream_cancel` may run during a read.
  Cancellation is permanent and the reader checks it at most every 50 ms while
  waiting for messages. The caller must wait for active reads before freeing
  the handle. Each stream accepts one reader at a time.
- Legacy `*_next` still returns 1 for a message, 0 for completion, and -1 for
  errors. Stream handles own their pool independently of the originating pool.
- Node local asynchronous tails run bounded native reads on worker threads and
  accept an `AbortSignal`. Go local tails use bounded waits to check context
  cancellation. Both retain one native stream instead of reopening it at wait
  expiry. Matching-message limits apply after tag filtering. Python local
  streams expose bounded waits and concurrent cancellation; high-level tails
  accept an optional `threading.Event` cancellation flag.

- Explicit local view batches require positive count and encoded-payload byte
  bounds. They wait for the first accepted message, then acquire available
  companions without waiting to fill. Zero per-call timeout polls; wait expiry
  returns an empty group, while completion/cancellation ends iteration. A first
  requested record above the byte bound fails without consumption; callers can
  retry with a larger bound. A later record failure returns the valid prefix
  and leaves the failing record for the next call. Sequence/gap checks,
  accepted-message limits and stream-wide deadlines remain per-record rules.
  Each returned message owns an independent snapshot. Ordinary reads acquire
  one record and keep no hidden prefetched queue.

- Local and HTTP tails deliver messages strictly by `seq` within a pool.
  Standalone UDP follows use the separate
  [arrival-order receive policy](../../udp/v1/SPEC.md#receive-ordering-and-notices).
- Streams support explicit caller cancellation.
- Implementations must respect backpressure and avoid unbounded buffering.
- Once cancellation is observed, no further messages may be delivered.
- A local tail cancellation flag ends the tail promptly and returns no further
  messages after cancellation is observed.
- HTTP reconnect behavior must be explicit and must not reorder messages.
  UDP follows never reconnect automatically.
- Remote one-shot network requests use a 30-second deadline in the Rust client.
  A transport timeout or incomplete successful-response body returns `Io`;
  an HTTP error response retains its status-based error mapping. System DNS
  resolution is not interruptible. Live tail bodies have no client request
  deadline; caller `TailOptions` and server limits still apply.
- The Rust remote client caps Lite3 responses, Lite3 tail frames, and individual
  JSONL tail lines at 256 MiB. It rejects an oversized Lite3 frame before
  reading its payload and ends a stream after a framing or size error.

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

### Playback timing

SDKs expose ordinary message streams. Applications own playback timing,
cancellation during delays, and any collection needed to protect a dataset from
later overwrite. CLI `follow --replay` remains available. The next major release
removes the earlier binding-specific replay APIs; ordinary stream operations and
the C ABI retain their contracts.

## Non-Contract Surface

- Binding-specific naming, argument ordering, and exact method/function signatures.
- Binding-specific prose examples and convenience helpers.

## References

- CLI contract: `spec/v0/SPEC.md`
- Remote protocol contract: `spec/remote/v0/SPEC.md`

## UDP send and follow

Standalone `UdpClient` / `UdpPool` APIs submit unconfirmed messages and follow
best-effort streams. They do not change `RemoteClient` confirmed append semantics.
`send_json` encodes the shared message shape to Lite3; `send_lite3` validates and
preserves complete bytes. Both return only local validation/OS submission success,
never destination sequence/time. A constructor resolves one UDP endpoint once;
open validates Name or a same-origin UDP Uri without claiming remote existence.
`UdpClient::parse_pool_url` splits a validated URL into endpoint and decoded name
without DNS or sockets; the CLI uses the same parser.

`UdpFollowOptions` chooses New, Last, Since timestamp, or Sequence and live/finite.
Follow events carry an owned exact-byte frame, an explicit server skipped
range/reason, or progress. The
[arrival-order policy](../../udp/v1/SPEC.md#receive-ordering-and-notices) describes
an unreleased amendment to the 1.x behavior; shipment requires a major release.
Clients deliver valid matching DATA in arrival order, including late messages
and duplicates.
Sequence/progress jumps must neither create inferred gaps nor suppress DATA.
Consumers choose whether to reorder or discard messages. Apply timestamp, tag,
and application filters to each message without creating skips. A finite follow
ends on terminal progress after draining accepted events; packets still in
flight may go unread. No ordering buffer, duplicate suppression, or data repair.
Explicit polling wait deadlines return no event; stream end has an independent
finished state.
A zero timeout polls the socket without blocking and still checks renewal and
liveness. Keep consuming to maintain the lease.
Close sends one best-effort stop; stopped consumers do not renew secretly.

The same behavior applies in C, Python, Node, and Go; preserve uint64 metadata.
`Pool::check_current_path` compares an open handle's native identity to its path
without writing. `Pool::into_path_guard` consumes the pool, releases its mapping,
and keeps the original file handle and path for the same read-only identity check.
Replacement/deletion ends the old UDP stream, including completion renewal.
Public `Gap` names denote explicit server skips for compatibility. `Unknown`
remains a reserved public reason; clients reject it in SKIP packets.
Wire behavior and network limits live in [UDP v1](../../udp/v1/SPEC.md).

### Point Lite3 frame metadata

`RemotePool::get_lite3_frame` returns sequence, nanosecond timestamp and original
payload from one HTTP response on current servers. It validates integer headers
and the requested sequence. Older servers without `plasmite-timestamp-ns` require
a separate JSON metadata lookup; that compatibility path does not provide one
atomic observation. `get_lite3` remains the payload-only operation.

### Validation observations

`LocalClient::validate_pool_observation` returns the existing `ValidationReport`
and optional retained bounds/count captured under the same shared pool lock as
validation. Counts describe observed retained messages, not a separate count of
verified frames. A failed validation leaves the count unknown. Later file changes
do not update these facts. `validate_pool` and the CLI machine report keep their
existing shape; human doctor output uses the captured observation without reopening
its path.
