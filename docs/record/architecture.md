# Architecture

This document explains how the Plasmite implementation is organized and why. The [2.0 contract](../../spec/v2/SPEC.md) defines the public CLI, library, HTTP, UDP and MCP behavior; the [spec index](../../spec/README.md) lists the versioned wire contracts.

## Design principles

Each principle states a condition that would violate it.

**Local-first.** The core layer must compile and pass all tests with no network dependencies. Pool operations never make network calls. A dependency in `src/core` that needs network access is a violation.

**Contract-first.** Public API types, CLI flag shapes and HTTP endpoint paths are frozen within a major version. Internal refactors that preserve observable behavior are always allowed. A change to observable behavior needs a spec update first. Merging a behavior change without the matching spec update is a violation.

**Shared core.** The CLI, API and HTTP server invoke pool and message operations through `src/core` or `src/api`. Duplicating correctness logic across interface adapters is a violation. Shared interface helpers (`pool_paths.rs`, `pool_info_json.rs`, `interface_wire.rs`) exist to prevent parallel implementations.

**Explicit failure modes.** Every error a public interface returns maps to a stable `ErrorKind`. A new failure case adds or reuses an `ErrorKind`. Returning a generic internal error for a condition that has a more specific kind is a violation.

## Layer model

```
Interface layer  →  Core domain layer  →  Platform layer
```

**Interface layer** (`src/main.rs`, `src/cli/`, `src/api/`, `src/serve.rs`, `src/http_v2.rs`, `src/mcp.rs`, `src/udp_input.rs`, `src/abi*.rs`):

- Parses and validates input, invokes core operations and maps results to user-facing formats.
- Uses core storage rules. For example, `plasmite doctor` calls `src/api/validation.rs` to assemble a report from the ring and frame checks in `src/core/validate.rs`.
- May hold bounded subscription state for selection, routing, leases, cancellation and finite completion. Readers own their durable progress.
- Releases pool mappings and cursors when delivery ends.

**Core domain layer** (`src/core/`):

- Owns storage writes, reads, reader positions, validation, sequencing and corruption detection.
- Never calls interface modules, makes network calls or depends on an async runtime.

**Platform layer** (`memmap2`, `libc`, `windows-sys`, plus the C shims in `c/`):

- Provides memory mapping, file locks, wake-ups and durability primitives.
- Contains no message semantics.

## Module map

The crate builds one library (`src/lib.rs`) and two binaries. `plasmite` (`src/main.rs`) adds the CLI and the server; `pls` (`src/bin/pls.rs`) runs `plasmite` with the same arguments. The C ABI and bindings use the library.

| Path | Role |
| --- | --- |
| `src/core/pool.rs` | Pool files: create, open, map, lock, append, delete. The only code that writes storage. |
| `src/core/plan.rs` | Pure placement planning for an append: where the frame goes and which old frames it drops. |
| `src/core/frame.rs` | The fixed 64-byte frame header, frame states and size rules. |
| `src/core/cursor.rs` | Walks committed frames in the ring; never yields a frame in `Writing` state. |
| `src/core/validate.rs` | Slow-path ring and frame checks for `doctor` and debug assertions. |
| `src/core/notify.rs`, `src/core/notify/` | Best-effort wake-ups using Linux shared-file waits, a Windows event per sequence, and macOS address waits or named semaphores. |
| `src/core/lite3/` | Safe wrappers over the vendored Lite3 C library (`sys.rs` is the raw FFI); JSON ↔ Lite3 conversion and message validation. |
| `src/core/error.rs` | `ErrorKind`, `Error` and the exit-code mapping. |
| `src/core/format.rs`, `ring_layout.rs` | Pool format versions; the ring map the web UI draws. |
| `src/api/` | The public Rust API. `client.rs` resolves `PoolRef`s and opens local pools; `message.rs` holds `feed`, `fetch`, `follow` and the shared reader state; `remote.rs` and `remote/` are the HTTP client; `udp.rs` is the UDP client; `access*.rs` and `browser_trust.rs` manage saved connections. |
| `src/view.rs`, `src/view/` | `MessageView`: an immutable Lite3 snapshot with borrowed node references, Serde support and JSON formatting. |
| `src/interface_wire.rs` | The serialized shapes every adapter shares: message envelope, pool info, bounds, metrics, error names. |
| `src/abi.rs`, `src/abi_*.rs`, `src/abi_borrowed/` | The C ABI in `include/plasmite.h`. `abi_views.rs` exposes snapshots, `abi_builder.rs` encodes runtime values, `abi_borrowed/` serves Go's private snapshots, `abi_remote.rs` adds HTTP pools, `abi_batches.rs` bounded batches. |
| `src/main.rs` | Process entry: parses arguments, builds `CliContext`, prints the final error and exits. |
| `src/cli/args.rs` | The complete clap syntax and help text. |
| `src/cli/mod.rs` | `dispatch`: routes each parsed command to its command-family module. |
| `src/cli/pool.rs`, `doctor.rs`, `feed.rs`, `stream.rs`, `server.rs`, `access.rs`, `utility.rs` | Command families: pool lifecycle; diagnostics; `feed` and `fetch`; `follow` and `duplex`; `serve`; `access`; `version`, `completion` and `mcp`. |
| `src/cli/message_stream.rs` | Splits stdin into complete messages (JSON, YAML, lines, Lite3) and writes framed Lite3 output. |
| `src/cli/*_support.rs`, `support.rs`, `output.rs`, `context.rs`, `result.rs` | Helpers for the command families, pool-reference parsing, JSON and human output, process-wide context and exit codes. |
| `src/serve.rs` | The axum server: local administration and web UI, HTTPS authorization, MCP over HTTP, the storage-operation budget. |
| `src/http_v2.rs` | The `/v2/pools` routes and the read worker that streams `fetch` and `follow`. |
| `src/secure_serve.rs` | Starts the loopback listener and the authenticated HTTPS listener together. |
| `src/udp_input.rs`, `src/udp_wire.rs` | The UDP server and the datagram codec it shares with `api/udp.rs`. |
| `src/mcp.rs`, `src/mcp_stdio.rs`, `src/oauth.rs` | The MCP dispatcher, its stdin/stdout transport, and OAuth for the HTTPS MCP endpoint. |
| `src/access_store.rs`, `browser_session.rs`, `server_private.rs`, `windows_private.rs` | Server identity, access keys and browser sessions, stored in protected files. |
| `src/serve_service.rs`, `src/serve_service/` | Installed servers: shared settings, then systemd and launchd (`unix.rs`) or the Windows Service Control Manager (`windows.rs`). |
| `src/serve_registry.rs`, `activity.rs`, `share_addresses.rs` | `serve status` discovery, current activity, and suggested addresses for the Access page. |
| `c/`, `vendor/lite3/` | The Lite3 shim, the Darwin wake-up bridge and the pinned Lite3 sources; see [building](../building.md). |
| `ui/` | Browser pages that `serve.rs` embeds. |
| `bindings/` | Python, Go and Node libraries over the C ABI; Node also ships a WebAssembly core for HTTP use. |

## One request end to end

`plasmite follow https://pools.example.net:9743/chat --tag error` shows every layer.

1. **CLI.** `main.rs` parses the arguments with `cli/args.rs` and calls `cli::dispatch`. `cli/stream.rs::follow` turns `--since` or `--last` into a `Read`, and `cli/support.rs::resolve_pool_target` classifies the reference as a local path, an HTTPS pool or a UDP pool.
2. **HTTP client.** `RemoteClient::open_pool` (`api/remote.rs`) checks that the pool exists. `stream_support::follow_remote` calls `RemotePool::follow_lite3` (`api/remote/read_v2.rs`), which sends `GET /v2/pools/chat/follow?tag=error` with the saved access key from `api/access.rs`. It asks for the binary Lite3 stream. The CLI can pass those bytes through for binary output or decode them for text output. On Unix, a watcher can cancel the request when the downstream reader closes stdout.
3. **Server.** `serve.rs` authorizes the key against `access_store.rs`, and `http_v2.rs::read` takes a permit from the `--max-tail-concurrency` budget. A `ReadWorker` on Tokio's blocking pool opens the pool and calls the same `Pool::follow_lite3` a local reader uses. It hands frames to the response through a 16-slot channel and returns the pool's identity in the `plasmite-incarnation` header.
4. **Core.** `api/message.rs` resolves the start position under the read lock with `core/cursor.rs`. Each pull copies the next committed frame, checks sequence continuity (a skipped sequence raises `RetentionGap`), then checks tags. At the end of the pool the reader waits through `core/notify.rs`, with bounded polling as the fallback.
5. **Back in the CLI.** Each frame reaches `emit_frame`, which writes JSON or Lite3 to stdout. If the reader falls behind retention, `report_read_gap` prints a notice and reopens at the reported `next_seq`.

The write side is shorter. `plasmite feed chat` on a local pool runs `cli/feed.rs` → `feed_support::ingest_from_stdin`. `message_stream::read_records` yields one complete message at a time, `record_payload` encodes it as Lite3, and `Pool::feed_lite3` (`api/message.rs`) calls `core/pool.rs::append_with_options`. That function takes the writer lock, asks `plan.rs` where the frame goes, writes and commits it, publishes the header and wakes waiting readers.

## Message representation

Storage holds Lite3 payloads plus a sequence and timestamp per frame. Every reader starts from one owned copy of a frame's bytes, taken under the storage lock (`core/pool.rs`). Overwrite, close or delete cannot change a message after the reader returns it, and the reader holds no lock while the caller works.

### Immutable message ownership

`MessageView` (`src/view.rs`) owns an immutable payload and the result of validating it. Lookups and iterators read Lite3 in place; a language adapter builds a wrapper or a requested scalar, never an eager native tree. Explicit conversion (`view/de.rs`, `view/json.rs`) builds ordinary containers or JSON text. A retained child (`view/owned.rs`) keeps its whole snapshot alive. Forwarding a view to another pool shares the payload and assigns a new sequence and time.

Each language holds snapshots its own way. C gets retained message owners and borrowed references (`abi_views.rs`). Rust borrows or takes an owned child. Python and Node wrappers keep the owner alive. Go keeps the immutable bytes and their validation proof in its own heap and passes them to `abi_borrowed/` on each call. Any mutable byte export copies.

Writers validate as they encode. `view/ser.rs` and `abi_builder.rs` read each caller value once, so a callback that mutates a value later cannot change bytes already encoded. Validation proof always travels with immutable bytes; a mutable buffer needs its own copy or validation.

Decoded messages, views and raw Lite3 are three outputs of one reader state in `api/message.rs`; the selection and loss checks run once for all three. Node's HTTP client runs the same checked core as WebAssembly and exposes the same read-only views.

### JSON output

C JSON readers validate the frame, borrow its tags, keys and strings, and format the envelope directly through Serde. Temporary entry lists keep key order deterministic, and byte fields stream base64 into the same writer. No yyjson document or Rust data tree is built.

Rust's object API returns `serde_json::Value`. Its decoder exports JSON and parses it with `float_roundtrip`, so JSON output and object reads both preserve the stored binary64 values exactly. Raw reads return the encoded bytes.

## Errors and wire shapes

`src/interface_wire.rs` holds one internal definition of the stable machine data: message envelopes, pool metadata, retained bounds, metrics, error-kind names and default error mappings. The library and the binary each compile this file privately, so sharing it adds no public Rust API.

A core `Error` owns the facts of a failure: kind, message, hint, path, sequence, offset and source chain. Adapters own presentation. The CLI chooses text or JSON and an exit code; HTTP chooses a status and response body; MCP chooses tool or JSON-RPC error content. A command's own message and hint win over the shared defaults. A `Permission` error can become HTTP 401 or 403 depending on the request's credentials; its kind stays the same.

HTTP handlers return errors through the shared response policy, and each handler keeps its order of authorization, extraction and storage admission visible in its own body.

## CLI structure

```
main.rs → cli::args + cli::dispatch → command-family modules → API/core
                                  ↘ CLI helpers + cli::output + cli::result
```

`main.rs` owns process concerns only. `cli/args.rs` owns syntax and help: root help orients the reader, the [CLI guide](../cli.md) explains shared rules, and command help describes each invocation. `dispatch` converts parsed variants into explicit argument structs for each command family. `CliContext` carries the pool directory, color mode and explicit output format; `CommandResult` carries the exit code back to `main.rs`. Command modules import the helpers they use by name. There is no command framework, service container or handler trait hierarchy, because plain functions with explicit arguments are easier to follow.

Argument modules never execute commands, command modules never exit the process, and CLI helpers never reimplement storage rules.

The CLI keeps `follow --one` so a one-message wait works the same in every shell. It watches its output pipe where the platform can detect a closed reader. On a retention gap it warns and resumes from `next_seq`; library readers raise the gap so the application decides whether its state needs rebuilding. `duplex` opens its reader before sending and keeps receiving after input ends. Command capture is an ordinary shell pipe into `feed --input-format lines`.

Binary output with tag filters checks metadata without decoding the data. JSON output decodes fully.

## Pool file layout

A pool file is `header | index_region | ring`.

- Header: metadata, bounds and offsets.
- Index region: optional fixed-size `(u64 seq, u64 offset)` slots.
- Ring: frames holding encoded `{meta, data}` payloads.

Invariants:

- Committed sequence numbers increase monotonically.
- Readers see a frame only after its commit state validates.
- Corrupt, torn or stale reads never return an invalid payload.
- An index mismatch falls back to a scan.
- A binary refuses a pool whose format version it does not understand, with an actionable error. `core/format.rs` lists the supported versions.
- A breaking layout change requires a new format version.

## Write and read paths

An append runs in this order (`core/pool.rs`):

1. Validate input and prepare the frame.
2. Acquire the writer lock and refresh the header.
3. Plan placement in the ring, including wrap and overwrite (`plan.rs`).
4. Commit the frame bytes.
5. Update the index slot, if the pool has an index.
6. Publish the header, release the lock, then wake readers.

Invariants:

- The writer holds the lock from planning through header publication and releases it before the wake-up.
- A frame left in `Writing` state never reaches a reader.
- `plan.rs` stays pure: no writes, and its output depends only on its inputs.

Deletion takes the same writer lock before removing the file, and an append checks that its path still names the locked file. An open handle to a deleted or replaced pool therefore cannot acknowledge a write that new readers would miss. These checks coordinate Plasmite processes; another program that renames files without the pool lock can still race them.

A point lookup takes a shared lock, captures the visible bounds, probes the index slot, falls back to a scan if the probe fails validation, and copies the payload before releasing the lock. Reads use the same OS lock as appends. A per-pool mutex serializes read-lock ownership because `flock` belongs to the shared file description. Scans do not copy skipped payloads. A corrupt committed frame returns `Corrupt` instead of retrying forever.

A reader resolves its start position when it opens; `fetch` also records the end boundary then. It checks sequence continuity before tags, so a filtered reader still reports a gap. `follow` defaults to new messages and `fetch` to retained history; both accept `since` or `last`. A library lookup is the first item of `fetch(since=seq)`, and CLI `fetch POOL SEQ` is the shell shortcut for it. Each feed, including a forwarded message, gets a new destination sequence and a wall-clock time.

Callers stop readers, choose deadlines, decide whether to retry a write and filter application fields. Plasmite owns position, tag matching and durability because it can apply them while it holds the pool.

### Wake-ups

On Linux, Windows and macOS 14.4 or later, a reader that reaches the end of the pool sets a bit in a reserved header word. The writer publishes its sequence and clears the bit under the lock, then wakes readers after unlocking if the bit was set. Readers still check the full sequence and their own position after waking.

- Linux waits on the header word.
- macOS waits on the full sequence with Apple's compare-and-wait call (`c/notify_shim.c`), so an older writer still wakes a newer reader. Older macOS releases use named semaphores.
- Windows readers open a named event for the file's identity and observed sequence before setting the bit. The writer signals the event for the sequence it replaced. Readers hold the event until they finish waiting, so a signal just before a reader parks still reaches it.

A bounded recheck recovers a commit if a writer exits before signalling or a wake-up fails. On Linux and Windows, cancellation can also wake a native C reader when the kernel supports the needed wait. Plain cancellation flags, C cancellation on macOS, and kernels limited to a single wait use a 50 ms check. Caller deadlines can shorten any wait.

Correctness never depends on a wake-up. A reader that never receives one can still read retained messages and detects gaps when writers overtake it. Removing wake-ups must leave non-timing tests passing.

### Reader handles

Rust and C readers share the reader state in `api/message.rs`. Rust readers borrow their pool; C streams own an independently opened pool, serialize their reader state and expose a separate atomic cancellation signal, so callers keep handles until active reads finish. Node workers share ownership through an active read, so closing a JavaScript wrapper cancels without freeing a worker's handle. A bounded wait can expire without ending the reader or losing its position.

## Transports

Every transport adapts requests into the same core calls. Network code and delivery policy stay outside `src/core`. Core helpers such as the native file-identity check serve every adapter and carry no transport state. A future transport must be another adapter, never a second correctness engine.

### HTTP

The HTTP adapter runs pool operations on Tokio's blocking pool. Each listener has separate permit pools for short storage operations, active message streams and MCP requests. All three use `--max-tail-concurrency` as their capacity. The `plasmite_follow` MCP tool also takes a stream permit. Admission to each pool happens immediately. When a pool runs out of permits, the operation returns `Busy`. That lets short operations such as feeds proceed while many follows wait for messages.

An admitted operation holds its permit until it exits, even if the client goes away. A `fetch` has a response deadline. A `follow` holds the pool open while it waits; if its output queue stays full past the timeout, the worker reports an error and releases its permit. Closing the response, revoking the key or stopping the server also ends the read.

JSON and binary streams end with an explicit record. `complete` ends a finite fetch; `error` carries a failure; `cutoff` carries the next unread sequence when a fetch hits the server's deadline. The client reopens at that sequence with the original end boundary and the pool's incarnation, so it never mistakes partial history for a finished fetch or reads from a replaced pool. A connection that ends without a final record raises `Io`.

An HTTP feed returns the committed message or a numeric receipt.

### UDP

`plasmite serve --udp` (`udp_input.rs`) routes each datagram to the ordinary append path. A UDP feed reports only that the datagram left the sender; it carries no receipt.

With `--udp-follow`, the server also serves reads. A subscriber first answers a cookie challenge, which proves the server can reach its return address before any pool opens. A bounded pool of workers then reads through core cursors and sends each message's sequence, timestamp and original Lite3 bytes. Subscribers renew a lease; close or expiry ends the subscription. A finite read releases its pool mapping after its final progress packet, and the receive loop keeps only a small completion record and a file-identity guard until the lease ends, so idle completed reads cost no thread or polling.

The client (`api/udp.rs`) delivers valid matching messages in arrival order, including late and duplicate packets. Only the server can report a known skip or retention gap. A jump in sequence numbers over UDP can mean loss or reordering, so the client delivers later arrivals without trying to repair the stream. The [UDP receive policy](../../spec/udp/v1/SPEC.md#receive-ordering-and-notices) defines these rules.

## Server state and access

Access records live beside the pool directory in protected `.plasmite-serve` state (`access_store.rs`). Authorization and revocation share one lock, so a revocation bars every later request; open streams hold a revocation flag for their key and stop when it changes. OAuth for the HTTPS MCP endpoint (`oauth.rs`) records only the key's ID, so revoking the key also ends its tokens. Saved native connections live under the current OS user (`api/access.rs`); disconnecting removes one destination without contacting the server.

On Windows, server state and saved connections share the helpers in `windows_private.rs`. Creation sets a protected access list for the token user, SYSTEM and administrators before writing secrets. Reads check owner and permissions through the open handle. The server holds the private-state directory and every parent folder open for its lifetime, and a saved-connection operation does the same for its own directory, so no one can swap a folder in the path while it runs. The helpers reject reparse points, hard links and filesystems without access lists. Saved connection values are also encrypted with the Windows Data Protection API (DPAPI) for the current user.

### Installed servers

`serve install` saves the `serve` arguments the owner typed in a `Setup` record (`serve_service.rs`). `effective_args` fills defaults and validates them each time a server starts or reports status, so defaults and validation live in one function and the saved job still reads the way the owner wrote it. Each job is named `net.plasmite.<hash of the pool directory>`.

- **Linux and macOS** (`serve_service/unix.rs`): a systemd unit or LaunchDaemon runs the server as the account that owns the pool directory, restarts it on failure and sets a private umask.
- **Windows** (`serve_service/windows.rs`): the Service Control Manager runs a copy of the executable from a protected directory under Program Files, as a per-pool virtual account `NT SERVICE\net.plasmite.<hash>`, so no password is stored. `server_private.rs` grants that account access to the server state. After a crash, the Service Control Manager restarts the server after 2, 5 and then 10 seconds. If you start and then stop the service while Windows has a crash restart pending, Windows can still run that restart. See [Start a server at boot](serving.md#start-a-server-at-boot). An update turns crash restarts off, stages the new executable in the unused of two install slots (`plasmite.exe`, `plasmite.alt.exe`), switches the service to it, and turns crash restarts back on only once the new server reports ready. If the new server fails to start, the update restores the previous executable, setup and identity. If that repair also fails, the service is stopped and disabled with its backups kept for an administrator.

## Extension rules

- New interfaces reuse `PoolRef` and the core message operations. Parallel pool resolution is a violation.
- New payload conventions keep the envelope (`seq`, `time`, `meta`, `data`). Removing or renaming an envelope field is a breaking change.
- Performance work keeps crash safety and the index fallback-to-scan path.
- A new command or flag must not change the behavior of existing commands.
- Shared code belongs with the invariant it enforces. Delivery and progress policy stay with the interface that owns them: the CLI decides how to report a gap, and browser pages own their displayed rows. A universal message or subscription model would add state without removing those responsibilities.
- Measure sparse reads, full conversion and forwarding separately, including allocation, retained owners and complete producer and consumer work.

## Quality criteria

Each claim is checkable by inspection or test.

- `src/core` has no `async` code and no network dependencies.
- Every public error condition maps to a named `ErrorKind`.
- The CLI, API and HTTP server run the same pool operations for equivalent actions.
- `plan.rs` has no side effects.
- Removing wake-ups fails no non-timing test.
- A pool with an unknown format version produces an actionable error, never a panic or silent access.
- The C ABI stays an adapter: argument translation, error mapping and stream orchestration, with no storage rules of its own.

## Guarantees and non-goals

Guaranteed:

- A stable error-kind surface and exit-code mapping.
- Pool file invariants survive crashes and restarts.
- The CLI, API and HTTP server behave the same for the same operation.

Not guaranteed:

- Hard real-time latency.
- Unbounded retention; pools are bounded rings.
- Compatibility for file details outside the versioned format.
