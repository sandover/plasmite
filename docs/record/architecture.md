# Architecture

This is the implementation architecture for Plasmite.
The [2.0 contract](../../spec/v2/SPEC.md) defines the public CLI, library,
HTTP, UDP and MCP behavior. The [spec index](../../spec/README.md) points to
the versioned wire contracts.

## Design principles

These are operational constraints, not labels. Each states a violation condition.

**Local-first.** The core layer must compile and pass all tests with no network dependencies. Pool operations must never make network calls. Any dependency introduced into `src/core` that requires network access is a violation.

**Contract-first.** Public API types, CLI flag shapes, and HTTP endpoint paths are frozen within a major version. Internal refactors that preserve observable behavior are always allowed. Changes that alter observable behavior require a spec update before the code change. Merging a behavior change without a corresponding spec update is a violation.

**Shared core.** CLI, API, and HTTP server must all invoke pool and message operations through `src/core` or `src/api`. Duplicating correctness logic across interface adapters is a violation. Shared interface helpers (`pool_paths.rs`, `pool_info_json.rs`) exist precisely to prevent parallel implementations; adding a parallel implementation is a violation.

**Explicit failure modes.** Every error returned by a public interface must map to a stable `ErrorKind`. Adding a new failure case requires adding or reusing an existing `ErrorKind`. Returning a generic internal error for a condition that has a more specific kind is a violation.

## Layer model

```
Interface layer  →  Core domain layer  →  Platform layer
```

**Interface layer** (`src/main.rs`, `src/cli/*`, `src/api/*`, `src/serve.rs`, `src/abi.rs`):
- Must: parse and validate inputs, invoke core operations, map results to user-facing formats.
- Must not: duplicate storage correctness logic across multiple interface surfaces.
- Allowed exception: one shared validation-report implementation may perform read-only ring/frame checks when exposed through API/CLI diagnostics.
- May retain bounded subscription state for documented selection, routing,
  leases, cancellation, and finite completion. Readers own durable progress.
- Must release pool mappings and cursors when data delivery ends.

**Core domain layer** (`src/core/*`):
- Must: own all storage writes, reads and reader positions, validation, sequencing, and corruption detection.
- Must not: call into interface-layer modules.
- Must not: make network calls or depend on a runtime async context.

**Platform layer** (`memmap2`, `fs2`, `libc`, `windows-sys`):
- Must: provide concurrency and durability primitives.
- Must not: contain message-semantic logic.

**Cross-layer rule:** Interface layers must not fork storage correctness logic into per-surface implementations. A single shared validator used by diagnostics is acceptable; embedding bespoke ring/frame/seq rules inside CLI handlers, HTTP handlers, and ABI wrappers independently is a violation.

## Interface representation and error ownership

Stable machine data has one internal representation in `src/interface_wire.rs`.
Message envelopes, pool metadata, retained bounds, metrics, error-kind names,
and default error mappings are defined there. The library and binary compile
that same source privately, so sharing these contracts does not add supported
public Rust API.

Core `Error` values own failure facts: kind, explicit message, hint, path,
sequence number, offset, and source chain. Interface adapters own presentation.
The CLI chooses text or JSON and an exit code, HTTP chooses a status and response
envelope, and MCP chooses tool or JSON-RPC error content. Explicit messages and
hints supplied by a command remain authoritative; shared policy supplies only
the stable defaults. Permission failures may map to HTTP 401 or 403 according to
request authorization context without changing their shared `Permission` kind.

## Immutable message ownership

Storage contains Lite3 payloads and separate sequence/timestamp identity. An
owned `MessageView` keeps an independent immutable payload and its validation
result. Pool reads copy under the existing storage lock, then release the lock;
UDP admission validates the received payload before delivery. Private checked
writers establish the same invariants as they encode and retain that result.
Forwarding shares immutable payload ownership while assigning a fresh identity.

A value reference identifies a node in that snapshot. Lookups and bounded
sequential iterators read Lite3 directly. Language adapters create wrappers or
requested scalars, not an eager native tree. Explicit native conversion builds
ordinary containers; JSON formatting writes text directly from stored values.
A retained child keeps its complete snapshot alive. C uses retained message
owners and borrowed references; Rust uses borrows or an owned child; Python and
Node wrappers retain owners; Go keeps private immutable bytes and proof in its
own heap. Mutable byte exports copy.

Decoded messages, immutable views and encoded Lite3 select different forms
of the same reader. The adapters share the selection and loss checks. Node's
HTTP adapter uses the checked core in bundled WebAssembly and offers the same
read-only view interface.

## Ownership and abstraction choices

Shared code belongs with the invariant it enforces. Storage and readability
checks have common owners; interface-specific progress and delivery policies
stay at their boundaries. The CLI decides how to report a loss and whether to resume.
Browser pages own displayed rows and progress; their shared reader handles
stream framing. A universal message or subscription model adds state without
removing these responsibilities.

Checked input encoders capture mutable values when they visit them. Later user
callbacks cannot change already encoded values. Retain validation proof with
immutable bytes; exposed mutable buffers require their own copy or validation.
Measure sparse reads, full conversion and forwarding separately, including
runtime allocation, retained owners and complete producer/consumer work.

Installed services preserve listener spelling where it identifies the native
job. Shared normalization owns defaults and validation; an additional settings
model must remove a concrete source of duplication to justify itself.

Ordinary HTTP handlers propagate errors through the shared response policy.
Each handler keeps its authorization, extraction and storage-admission order
explicit. Cancellation and revocation retain their existing owners. Binary CLI
tag filters reuse metadata/readability checks without constructing unused data
trees. JSON output requests full decoding.

## CLI ownership

The `plasmite` binary uses ordinary Rust modules with one-way dependencies:

```
main.rs → cli::args + cli::dispatch → command-family modules → API/core
                                  ↘ CLI helpers + cli::output + cli::result
```

`main.rs` owns process concerns only: normalized argument parsing, construction
of the small `CliContext`, final error presentation, and process exit.
`cli::args` owns the clap syntax and help contract. Root help
orients the reader, the CLI guide explains shared operating rules, and command
help describes each invocation. These surfaces should form one complete manual
with clear ownership. `cli::dispatch` converts
parsed variants into explicit argument structures for these command families:

- `pool` and `doctor` for lifecycle, metadata, and diagnostics;
- `feed` for writing, and `fetch` for finite history and exact lookup;
- `stream` for follow and duplex;
- `server` and `access` for serving and connection commands;
- `utility` for version, completion, and MCP stdio.

`CliContext` contains the resolved pool directory, color mode, and explicit
JSON presentation choice.
`CommandResult` carries the selected exit code back to `main.rs`. The larger
command families use focused helper modules: `doctor_support`, `pool_support`,
`feed_support`, and `stream_support`. Shared parsing, pool
references and payload conversion live in `cli::support`. CLI
presentation helpers live in `output_support`; JSON output lives in
`cli::output`. Command modules import the helpers they use by name. There is
no command framework, service container, or handler trait hierarchy.

Invariant: argument modules do not execute commands, command-family modules do
not terminate the process, and shared CLI helpers do not reimplement storage
semantics owned by the API and core layers.

Server access records live beside the pool directory in protected
`.plasmite-serve` state. Authorization and revocation share one lock, so a
successful revocation bars later requests. Remote tails hold a revocation flag
for their key and stop when it changes. Native saved connections live under
the current OS user; disconnect removes one destination without contacting
the server.

On Windows, server state and saved connections share the private filesystem
helpers in `windows_private.rs`. Creation sets a protected access control list
for the effective token user, SYSTEM, and administrators before writing secrets.
Reads check the owner and permissions through the open file handle. Directory
handles hold the path in place during a server's lifetime or a saved-connection
operation. The helpers reject reparse points, hard links, and filesystems that
do not enforce access control lists. Saved connection values also use Windows
Data Protection API (DPAPI) encryption for the current user.

## Data model and on-disk layout

A pool file is:

`header | index_region | ring`

- Header: metadata, bounds, and offsets.
- Index region: optional fixed-size seq→offset slots (`(u64 seq, u64 offset)`).
- Ring: append log frames containing encoded `{meta, data}` payloads.

Key invariants:

- Sequence numbers are monotonically increasing for committed messages.
- Frame commit state is validated before exposure to readers.
- Corrupt, torn, or stale reads do not silently return invalid payloads.
- Index mismatches always fall back to scan for correctness.
- A binary must refuse to open a pool with a format version it does not understand; an unknown version produces an actionable error, not a panic or silent data access.
- Format version increments are additive within a major version where possible; breaking changes to the on-disk layout require a new format version.

## Write/read paths

The storage reader returns an owned Lite3 frame. Interface adapters choose the
representation their caller requests. C JSON readers validate the immutable
frame, borrows its tags, keys and string values, and formats the envelope
directly through Serde. It uses temporary collection-entry lists to retain
deterministic key order. Native byte fields stream base64 into that same writer.
This path builds neither a yyjson document nor a Rust application-data tree.

Rust's object API still requests an owned `serde_json::Value`. Its current
decoder exports data JSON and parses that final tree with `float_roundtrip`
enabled. Both JSON output and object reads preserve stored binary64 values;
an extra parser's rounding does not define the value. Raw reads retain their
encoded bytes. Every route keeps the existing snapshot copy under the storage
lock, so pool overwrite or close cannot change a returned message.

Feed writes a record in this order.

1. Validate input and prepare frame.
2. Acquire writer lock.
3. Plan placement in ring (including overwrite/wrap decisions).
4. Commit frame bytes.
5. Update index slot (when enabled).
6. Publish header updates and notify waiters.

On Linux, Windows, and macOS 14.4 or later, a reader that reaches the end
of the pool arms a bit in the mapped header. The writer publishes its sequence
and clears the bit under the lock. It wakes readers after unlocking when the bit
was set. The notification word uses reserved header space. Readers still check
the full sequence and their own position in the held pool.

Linux waits on the notification word. macOS waits on the full sequence with
Apple's public compare-and-wait operation, which lets an older writer still
signal a new reader. On Windows readers open a named event for the held
file's identity and full observed sequence before they arm the bit. The writer
signals the event for the sequence it replaced. Readers retain the event until
they finish waiting, so a signal just before a reader parks still reaches it.

A bounded recheck recovers commits if a writer exits before signalling or a
hint fails. Linux and Windows can also wake a native C reader on cancellation
when the kernel supports the required wait. Plain cancellation flags still
need a 50 ms check, as do C cancellation on macOS and kernels that fall back
to a single wait. Caller deadlines can shorten any wait. Older macOS releases
keep the semaphore fallback.

Invariants:

- The writer holds the lock from placement planning through header publication,
  then releases it before sending the optional notification.
- A frame in `Writing` state that is never committed must not be returned to readers.
- `plan.rs` must remain pure and side-effect-free. It must not write to the pool file. Its output must be fully determined by its inputs.

Deletion acquires the same writer lock before removing the file. An append
checks that its path still names the locked file before it changes storage.
An open handle to a deleted or replaced pool therefore cannot acknowledge a
write that new readers would miss. These checks coordinate Plasmite operations;
other programs that rename files without the pool lock can still race them.

A point lookup can use the storage index to seek.

1. Acquire a shared file lock and capture visible bounds.
2. Validate requested seq and probe the index slot.
3. If probe fails validation, fall back to scan under the same lock.
4. Copy the selected payload into an owned frame before releasing the lock.
5. Decode the owned payload into the message envelope.

Read snapshots use the same OS lock as appends. A per-Pool mutex serializes
read-lock ownership on the shared file description. Returned frames own their
payload bytes, so retaining a frame cannot observe a later overwrite. Internal
frame views remain inside the lock lifetime; scans do not copy skipped
payloads. Corrupt stable frames return `Corrupt` rather than retrying forever.

The core resolves a position and, for fetch, records the end boundary when
the reader opens. It checks sequence continuity before checking tags. Follow
waits for commits, using notifications and bounded polling.

Invariant: Correctness of the tail path must not depend on notify delivery. Notify is a latency optimization; a tail that never receives a notification must still eventually return all committed messages. Removing notify must not cause failures in non-timing tests.

Rust and C tails share private reader state with an explicit pool input. Rust
tails borrow their pool; C streams own an independently opened pool. Encoding
adapters select decoded messages or raw frames. A bounded wait can expire without
ending the reader or losing its sequence checkpoint. C streams serialize reader
state and expose a separate atomic cancellation signal; callers retain handles
until active reads finish. Node workers hold shared ownership through an active
read, so closing a JavaScript wrapper cancels without freeing a worker's handle.
Native JSON and Lite3 streams enable the same best-effort notifications as Rust
tails. They retain bounded polling when notification setup or waiting fails.

A read opens at a position. `follow` defaults to new messages, and `fetch`
defaults to retained history. Either accepts `since` or `last`. A library
lookup takes the first item of `fetch(since=seq)`. CLI `fetch POOL SEQ` adds the
shell shortcut for that lookup. Sequence and time are exact integers in code.
A sequence belongs to one pool. Each feed, including a forwarded message,
gets a new destination sequence and a wall-clock timestamp.

Callers stop readers, choose deadlines, decide whether to retry a write and
filter application fields. Plasmite owns position, tag matching and durability,
because it can apply them while accessing the pool. The CLI keeps `follow --one`
to make a one-message wait portable across shells. It watches its output pipe
where the platform can detect a closed reader. It warns about a retention gap
and resumes from the reported next sequence. Library readers raise the gap so
the application can decide whether its state needs rebuilding. UDP only
reports the losses the server can announce.

`duplex` opens its reader before sending and keeps receiving after input ends.
Command capture uses an ordinary shell pipe into `feed --input-format lines`.

## Transport architecture

Plasmite is transport-agnostic at the core.

- Local mode: CLI/API calls directly into core operations.
- Remote pool control: `plasmite serve` adapts HTTP and HTTPS requests into
  the same core calls.
- Optional UDP: the same server routes input to the existing append operation.
  With `--udp-follow`, bounded workers read through core cursors and send
  sequence, timestamp, and original Lite3 bytes to subscribers. A cookie proves
  return reachability before a worker opens a pool. Active workers hold their
  opened pool and cursor. A finite worker returns after terminal progress and
  releases its mapping. The receive loop keeps a bounded completion record,
  a file identity guard, and the existing lease. Short jobs check
  identity and answer renewals without blocking the receive loop. Close or
  expiry removes the record; idle completed reads keep no worker or polling loop.
  The [receive policy](../../spec/udp/v1/SPEC.md#receive-ordering-and-notices)
  requires arrival-order delivery of valid matching messages, including late
  messages and duplicates; consumers choose whether to reorder or discard them.
  Only explicit server skips produce gap reports; DATA/progress sequence jumps
  never suppress a message or establish loss. Opening and normal reception share
  packet validation and event handling; their wait and retry rules stay separate.
  The client performs no data repair. Public `Skip`/`udp-skip` names describe server
  skips for compatibility; the reserved `Unknown` value is not a wire skip reason.
  UDP feed reports local submission success. HTTP feed returns a
  committed message or a numeric receipt. The private wire codec serves the CLI/server and library.
- Future transports must be adapters over the existing core, not alternate correctness engines.

Adapters use the core's storage rules. Keep network operations and delivery policy outside `src/core`. Core helpers such as the read-only native file identity check apply to every adapter; they carry no transport-specific state.

The HTTP adapter runs synchronous pool operations on Tokio's blocking worker pool. A semaphore admits at most `--max-tail-concurrency` ordinary storage operations at once; reusing this existing server budget avoids a second tuning surface. Admission is immediate. A request that arrives when every permit is in use receives the shared `Busy` error instead of waiting in an unbounded queue. Long-lived tail streams and MCP waits retain their specialized concurrency path, so ordinary storage pressure does not occupy an async runtime worker or prevent health and MCP routing from making progress.

Once admitted, a blocking storage operation holds its permit until it exits, even if the client cancels the request. HTTP fetches have a response deadline. An HTTP follow holds the same pool open while it waits for messages. If its bounded output queue stays full for the timeout, the worker ends the read and releases its permit. Closing the response or stopping the server also ends the read.

HTTP JSON and binary readers carry the same end states. Complete ends a finite fetch, error carries the failure, and cutoff carries the next unread sequence when a fetch reaches the server's response deadline. A client opens a new request at that sequence and keeps its original fetch boundary. A connection that ends without a final record raises `Io`. This prevents partial history from looking like a finished fetch, including when the client filters by tags.

## Extension seams

- New interfaces must reuse `PoolRef` and core message operations. Parallel pool-resolution logic is a violation.
- New payload conventions must preserve existing envelope semantics (`seq`, `time`, `meta`, `data`). Removing or renaming envelope fields is a breaking change.
- Performance work must preserve crash-safety and fallback correctness paths. Optimizations that bypass the index fallback-to-scan path are violations.
- New commands or flags must not change the observable behavior of existing commands as a side effect.

## Quality criteria

These are auditable claims about the implementation. Each is checkable by inspection or test.

- `src/core` contains no `async` code and no network dependencies.
- All public error conditions map to a named `ErrorKind` variant; no public path returns a generic internal error for a condition with a more specific kind.
- All three interface surfaces (CLI, API, HTTP) exercise the same underlying pool operations for equivalent actions.
- `plan.rs` has no side effects; its output is fully determined by its inputs.
- The notify path is never on the correctness critical path; removing notify must not cause failures in non-timing tests.
- Pool files opened with an unknown format version produce an actionable error, not a panic or silent data access.
- The C ABI (`abi.rs`) remains adapter-focused: parameter translation, error mapping, and session/poll orchestration only; it must not introduce separate storage correctness semantics from `src/core`/`src/api`.

## Operational guarantees vs non-goals

Guaranteed:

- Stable error-kind surface and exit-code mapping.
- Durable pool file invariants under normal crash and restart scenarios.
- Cross-surface behavior parity (CLI/API/HTTP) for the same operation class.

Not guaranteed:

- Hard real-time latency.
- Infinite retention (ring buffers are bounded).
- Backward compatibility for undocumented internal file details outside versioned formats.
