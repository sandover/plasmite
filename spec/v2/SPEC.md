# Plasmite 2.0 contract

This contract covers the 2.0 CLI, libraries, HTTP, UDP and MCP. The corresponding 1.x specifications continue to describe 1.x. Plasmite 2.0 keeps the on-disk pool format and stored message envelope, so old pool files remain readable and writable without migration. The source APIs, native binary interface, HTTP protocol and CLI binary stream format make a major-version cutover.

## Common operations

- A client selects one local directory or remote server. The default local directory is `.plasmite/pools` in the user's home directory. A bare name resolves within that directory. The CLI and bindings that accept string references recognize a native path separator as a file reference. An HTTPS pool reference selects a server. Plasmite's native HTTP clients can use a saved connection. A UDP reference selects the datagram transport and the subset described under [UDP](#udp).
- `open_pool(ref)` returns a pool handle or `NotFound`. `open_pool(ref, create=true)` creates a missing pool at the default size of 1 MiB (1,048,576 bytes) and leaves an existing pool's size unchanged. `create_pool(ref, size)` returns a pool handle and reports `AlreadyExists` for an existing pool. Both work for local and HTTPS references.
- `pool.info()` and `list_pools()` describe the reference, file size, and first and last retained sequences. Pools can have `about` text and an `example` JSON object that describe their intended use. Each can use up to 1,024 UTF-8 bytes. Plasmite stores both with the pool and treats the example as guidance. Empty pools have no bounds. Each result describes what the inspection saw and can go stale after it returns. Diagnostic fields, such as message counts and utilization, may accompany the shared fields. `delete_pool` waits for an active feed to finish, needs permission to open the pool for locking, and reports `Busy` when it can't remove the pool safely. Releasing a handle leaves the pool on disk.
- `feed(value, tags, durability)` accepts an object, explicitly identified JSON or Lite3 bytes, or a `Message`. JSON bytes hold an application object. Lite3 bytes and `Message` inputs forward data and tags, and the destination supplies its own metadata. Supplying tags with a forwarded message produces `Usage`.
- A feed assigns a new sequence and samples system wall time, then returns the committed `Message`. `fast` is the default durability. `flush` waits for storage to flush before a successful return. Writes never retry automatically. An I/O failure can arrive after the message is already in the pool.
- The envelope has `seq`, `time`, `meta` and `data`. `meta` contains `tags` and, for writes authenticated with a server access key, `sender`. The server sets `sender` to the key name and discards metadata supplied by the caller. Direct file, local listener and UDP writes leave `sender` absent. Sequence starts at 1 and defines identity and order within a pool. Libraries expose one `time` field in Unix nanoseconds on messages, views, and encoded frames. Text uses UTC RFC 3339 with exactly nine fractional digits. Time can repeat or go backward. A forwarded message gets the destination's sequence and time.

## Errors

Plasmite operations use these error kinds across interfaces: `Usage`, `NotFound`, `AlreadyExists`, `Busy`, `Permission`, `Corrupt`, `Io`, `RetentionGap` and `Internal`. Bindings can reject arguments before calling Plasmite. For example, Python raises `ValueError` for a pool name containing a null byte and `TypeError` for invalid `feed` input. Callers bound waits through cancellation or the binding's wait options. A `RetentionGap` carries `missing` and `next_seq` as described under [Reads](#reads).

## Reads

`follow` and `fetch` return a reader. Opening selects the reader's position before returning to the caller, including in asynchronous APIs. If a caller writes a message after opening a follow and before pulling its first item, the follow delivers that message when it meets the selection.

| Selection | Meaning |
| --- | --- |
| Omitted | `follow` starts at the next unwritten sequence. `fetch` starts at the first retained sequence. |
| `since=oldest` | Start at the first retained sequence, or at the next unwritten sequence if the pool is empty. |
| `since=SEQ` | Start at sequence SEQ, inclusive. SEQ is a positive integer. A value above the next unwritten sequence produces `Usage`. A value below the first retained sequence raises `RetentionGap` before the reader yields anything. |
| `since=TIME` | Start at the first retained record, in sequence order, whose stored time is at or after TIME. Later records follow in sequence order, including records with earlier times after a clock step. If no retained record qualifies, start after the last sequence present at opening. |
| `last=N` | Start N records back from the last record present at opening. Zero starts at the next unwritten sequence. |

A read accepts at most one of `since` and `last`; both produce `Usage`. A duration resolves once, relative to the opening time. `last` counts records before the tag filter. A record passes the tag filter when it carries every requested tag exactly.

A time start never reports loss at opening. The pool can't know whether records it already dropped had times inside the requested window, so `since=TIME` on a pool whose history is shorter than the window starts at the first retained record without a `RetentionGap`. Loss detection applies from the resolved start onward.

`fetch` stops at the last sequence present at opening, even while writers continue. A fetch whose start is the next unwritten sequence returns an empty reader.

A local or HTTP reader checks sequence continuity before it applies the tag filter, so a filter can't hide loss. When the reader finds that the pool dropped part of its range, it raises `RetentionGap` before yielding any later record. `missing` is the first missed sequence and `next_seq` is the first sequence still retained when the reader detected the loss. A caller that accepts incomplete history reopens at `next_seq`. CLI `fetch` reports the error and exits nonzero; CLI `follow` reports it on stderr and resumes at `next_seq`.

### Stopping a read

The caller stops a read by closing or dropping the reader, or by cancelling through a supported signal. Use the binding's cleanup mechanism when you leave the loop early. Cancellation keeps messages already returned. Library read options leave counts, idle timeouts and gap policy to the caller. The CLI offers `follow --one` to wait for a single matching message. C, Python and JavaScript readers offer a per-call wait for the next record, which can expire while the reader stays open. Python and JavaScript accept seconds, and Python also accepts a `timedelta`. C takes milliseconds. Go passes a context to each read.

### Checkpoints

A checkpoint belongs to one pool. After completing sequence N, resume by opening at N+1. Discard checkpoints when a pool is replaced. Pool information serves inspection; don't advance a checkpoint from it.

## Binding names and representations

| Operation | Python | JavaScript | Rust | Go | C |
| --- | --- | --- | --- | --- | --- |
| Create | `create_pool` | `createPool` | `create_pool` | `CreatePool` | `plsm_pool_create` |
| Open | `open_pool` | `openPool` | `open_pool` | `OpenPool` | `plsm_pool_open` |
| Info | `pool.info` | `pool.info` | `pool.info` | `pool.Info` | `plsm_pool_info` |
| List | `list_pools` | `listPools` | `list_pools` | `ListPools` | `plsm_pool_list` |
| Delete | `delete_pool` | `deletePool` | `delete_pool` | `DeletePool` | `plsm_pool_delete` |
| Store | `feed` | `feed` | `feed` | `Feed` | `plsm_pool_feed` |
| Live read | `follow` | `follow` | `follow` | `Follow` | `plsm_pool_follow` |
| Finite read | `fetch` | `fetch` | `fetch` | `Fetch` | `plsm_pool_fetch` |

Python and JavaScript provide default-directory module functions and explicit clients. Python opens reads synchronously. JavaScript resolves a Promise after the read opens. Python exposes `RetentionGapError.missing` and `next_seq` and cancels through an event or by closing the reader. JavaScript uses `missing` and `nextSeq` and cancels through an `AbortSignal` or the async iterator's `return`. Both use `plasmite.at(t)` to mark a numeric nanosecond instant, because a bare number means a sequence. Rust uses `Read` and `Feed` option builders, a typed instant selection, and an explicit cancellation flag or dropping the reader. Go uses option functions including `Since`, `Last` and `Tags`, with `At` to mark a nanosecond instant and context cancellation. Rust and Go expose the gap fields with their native casing. C uses size-tagged option structs, an explicit discriminator for the kind of `since`, separate gap fields on the error, reader cancel and explicit free.

Python applications can also use `plasmite.aio` for local, HTTPS, and UDP pools. An async reader opens when code enters its context or requests its first item. A timeout or task cancellation leaves the reader open. A second concurrent read raises `UsageError` and leaves the first pending. Exit the reader's context or call `aclose()` to close it.

A successful stored feed and each reader item return the same message type. The message owns an immutable snapshot of the stored Lite3 envelope and survives both reader and pool closure. Access to sequence, time, tags, and sender uses that snapshot without decoding `data`. The first full `data` access decodes and caches the application object. C borrows data as a value reference and can write JSON on demand. Local reads copy a retained record into an owned snapshot. UDP sends return once the socket accepts the datagram because they have no stored result. The synchronous UDP reader also exposes transport events.

| Interface | Explicit encoded input | Raw output |
| --- | --- | --- |
| Python | `format="json"` or `format="lite3"` | `message.raw`, a read-only `memoryview` |
| JavaScript | `format: "json"` or `format: "lite3"` | `message.raw`, an independent byte buffer |
| Rust | `FeedInput::Json(bytes)` or `FeedInput::Lite3(bytes)` | `message.raw()`, a borrowed byte slice |
| Go | `api.JSON(bytes)` or `api.Lite3(bytes)` | `message.Raw()`, an independent byte slice |
| C | `PLSM_FEED_JSON`, `PLSM_FEED_LITE3`, or `PLSM_FEED_MESSAGE` | A byte span borrowed from the owned message |

Use `field` in Python, JavaScript or Rust to read a single field without decoding the rest. Go offers `Select` for individual paths. Use the message itself to forward it as a new feed. If you only need the sequence or the stored bytes, leave data undecoded. The [upgrade guide](../../docs/record/upgrading-2.0.md) maps former operation variants to these calls. The [language-interface performance record](../../docs/performance/language-interfaces.md) covers the local Rust read, forward and append workloads and links to the per-binding measurements.

## Native library

The 2.0 C library carries its major version in its file name, so a program built for 1.x can't load it by accident:

| Platform | 1.x | 2.0 |
| --- | --- | --- |
| Linux | `libplasmite.so` (soname `libplasmite.so`) | `libplasmite.so.2` (soname `libplasmite.so.2`) |
| macOS | `libplasmite.dylib` (install name `@rpath/libplasmite.dylib`) | `libplasmite.2.dylib` (install name `@rpath/libplasmite.2.dylib`) |
| Windows | `plasmite.dll` | `plasmite2.dll`, with import library `plasmite2.lib` |

The header defines `PLSM_ABI_MAJOR 2`, and the library exports `uint32_t plsm_abi_major(void)`, which returns 2. Python and Go load the versioned file. The npm package places that same versioned library next to the Node add-on, which links dynamically against its versioned name. All three check `plsm_abi_major()` at load before using other Plasmite calls. A mismatch fails with a clear error. The versioned filenames allow 1.x and 2.0 libraries to coexist.

## CLI

- `feed POOL [OBJECT]`, `follow POOL` and `fetch POOL [SEQ]` use the shared operations. `--create` asks `feed`, `follow` or `duplex` to create a missing pool at the default size, for local and HTTPS pools. `pool create --size SIZE` sets a custom size. `pool create --about TEXT --example JSON` sets the optional description and example. `pool describe NAME` replaces both, with omitted fields cleared. `pool info` and `pool list` show descriptions.
- `--since` accepts a decimal sequence, `oldest`, RFC 3339 or a relative duration. `follow` and `fetch` also accept `--last N` or `-n N`, and a repeatable `--tag TAG`.
- `follow --one` exits successfully after writing the first matching message on every platform. With the default start, it waits for a new message. On Linux and macOS, local follow also exits successfully when its downstream command closes the pipe, even while the pool is quiet. Use `--one` for scripts that must work across shells, including PowerShell pipelines that keep the pipe open after taking one message.
- `fetch POOL SEQ` requests exactly one sequence. It prints that record, reports `RetentionGap` if retention removed the record, reports `NotFound` if SEQ is the next unwritten sequence, and reports `Usage` if SEQ is zero or beyond the next unwritten sequence. With local and HTTP library reads, take the first item of `fetch(since=SEQ)`. An empty reader means SEQ isn't written yet. For UDP, the requested datagram may go missing in transit, and the CLI reports `NotFound` if the subscription completes without receiving it.
- `feed` accepts a positional object or stdin. Text with exactly `seq`, `time`, `meta` and `data` counts as a forwarded message if `meta` is an object containing `tags`. Plasmite keeps `data` and `tags` when forwarding. Other objects are application data. The source `seq` must be positive and the source `time` may use any valid RFC 3339 form. A malformed reserved envelope fails with `Usage`, and `--tag` with a forwarded message produces `Usage`. By default `feed` recognizes JSON objects, marked YAML documents and framed Lite3 from their syntax; `--input-format` selects `json`, `yaml`, `lite3`, `lite3-raw` or `lines` explicitly.
- `--input-format lines` stores one `{"line":"..."}` object per input line. A line ends at `\n`; a trailing `\r` is removed. A final line without a newline is stored. Input that isn't valid UTF-8 fails with `Usage` at the offending line, after the earlier lines are stored.
- `follow`, `fetch`, and `duplex` default to `compact` in a terminal. Each line shows the local time, sequence, sender if present, tags, and compact JSON data, trimmed to fit the terminal. To see the complete message in a terminal, select `--format pretty` or `-o pretty`. Output to a pipe or file defaults to one complete JSON message per line. Diagnostics go to stderr. `--format lite3` writes the [binary stream](#binary-stream) without control records. The exit status reports how the read ended.
- `duplex` opens its receiver before sending and uses the shared feed and follow options where they apply.

## HTTP

The pool collection is `/pools`, and `/pools/{pool}` names a pool. Every HTTP response carries `Plasmite-Version: 2`.

- POST on the collection takes `{"pool":"events","size_bytes":1048576}` and returns `{"pool":…}`. The size defaults to 1 MiB, and the request can include `about` and `example`. `PUT /pools/{pool}/description` replaces both and returns `{"pool":…}`. GET on the collection returns `{"pools":[…]}`. GET on a pool returns `{"pool":…}` with an `incarnation` that identifies the current pool file. DELETE returns `{"ok":true}`. POST `/pools/{pool}/open` takes `{"create":false}` and returns `{"pool":…}`, creating the pool if `create` is true and the pool is missing.
- GET `/pools` with `Accept: text/event-stream` streams `pools` events with a snapshot on connection and whenever the pool list or pool state changes. Each snapshot contains the `pools` array with ring layouts and an `available` flag. The browser uses the layouts to draw messages in the pool map.
- POST `/pools/{pool}/feed` takes an object-valued `data` with optional `tags` and `durability`. Use `Content-Type: application/x-plasmite-lite3` to send the raw Lite3 payload, with optional `durability` in the query. Either form returns `{"message":…}` by default. With `Accept: application/x-plasmite-lite3`, the body carries the stored Lite3 bytes, and decimal `Plasmite-Seq` and `Plasmite-Timestamp-Ns` headers carry the new identity.
- GET `/pools/{pool}/follow` and `/pools/{pool}/fetch` read with `since`, `last` and repeated `tag` query parameters. Both resolve the opening position before the successful response reaches the caller.
- The [1.x authentication rules](../remote/v0/SPEC.md#authentication--access) still apply, with the browser session at `/browser/session` and its cookie at `Path=/`. Access keys default to full access. A read-only key can list and read the served directory. Writes, pool creation, description changes, deletion and opening with `create=true` require a full-access key. The server caps request bodies at 1 MiB by default (`--max-body-bytes`). The server limits short storage operations, finite fetches, ordinary MCP requests, and live reads separately. HTTP follows, MCP follow calls, and MCP event streams share the live-read limit on each listener. The server uses a 30-second deadline for opening a read, delivering a fetch response, or waiting for a stalled follow to make progress.

Upgrade the server and clients together. A 2.0 server answers the old versioned pool routes with HTTP 426 and an error naming the required protocol version. A 2.0 client treats a 404 from a pool route without `Plasmite-Version: 2` as a server version mismatch.

### Stream endings

An HTTP follow keeps the same pool open, including while it waits for new messages. If the output queue stays full for the server's timeout, the server stops reading the pool and frees the stream slot. It sends an `Io` error after the queued messages if the connection can still deliver the error.

When the server ends a read and the connection can deliver the ending, it sends exactly one control record.

| Type | Meaning | Client library behavior |
| --- | --- | --- |
| `complete` | A fetch reached its upper bound. | Ends the reader normally. |
| `error` | The read failed. Carries the standard structured error, such as `RetentionGap` with `missing` and `next_seq`. | Raises the error. |
| `cutoff` | A fetch reached the server's time limit. Carries `next_seq`, the next unexamined sequence, and `until`, the original upper bound. | Sends a new fetch request with `since=next_seq` and the same `until`. |

The server sends every matching record before it advances past it, so a cutoff never skips one. The next request uses the cutoff's `next_seq` even when the response carried no matching records. A continued fetch keeps its original upper bound. The HTTP protocol uses `until` only to continue a fetch. A cutoff never hides a retention gap or turns incomplete history into a successful completion. A response that ends without a control record raises `Io`, unless the caller cancelled or closed the reader.

Each successful read response carries `Plasmite-Incarnation`, the opaque identity of the opened pool. A client sends that identity as the `incarnation` query parameter when it continues a fetch. The server checks it before validating the resumed range, and the client checks the response identity before accepting records. A different identity ends the read with `NotFound`. A continuation with a missing identity raises `Io`.

Fetching one record over HTTP opens a fetch and closes it after the first record. Closing a response early stops the server's work for that read promptly.

### JSON stream

A JSON stream uses `Content-Type: application/jsonl` and carries one object per line. Message records are envelopes. Control records have a top-level `type` of `complete`, `error` or `cutoff`. For `error`, the structured details appear in an `error` field.

### Binary stream

A binary stream is a sequence of records, each starting with a one-byte kind and a four-byte big-endian body length:

| Kind | Body |
| --- | --- |
| `0x4D` (`M`), message | `[u64be seq][u64be time_ns][payload]`, where the payload is the stored Lite3 message |
| `0x43` (`C`), control | UTF-8 JSON with the same fields as the JSON control record |

Request the HTTP binary stream with `Accept: application/x-plasmite-lite3-stream`. The response uses the same media type and starts directly with records. CLI binary streams start with the four ASCII bytes `PLS2` and carry message records only. An empty fetch writes the prefix alone. An empty feed or follow that ends before delivering a record can leave stdout empty. `feed --input-format lite3` reads the CLI format and rejects control records.

## UDP

`feed` submits a datagram and returns no stored receipt. `follow` and `fetch` accept every selection in [Reads](#reads). A UDP `fetch` is a finite subscription that ends when the client receives terminal progress or an error. UDP doesn't offer pool lifecycle or inspection. Reads deliver received messages in arrival order, including duplicates and late messages, alongside the server's skip and progress events. A server-announced retention gap rejects or ends the subscription with `RetentionGap`, `missing` and `next_seq`. A completed UDP fetch can miss datagrams without an error. The [UDP transport spec](../udp/v1/SPEC.md) defines wire limits, skip reasons, leases and the receive policy.

## MCP

Local stdio and direct HTTPS register the same tool schemas and use the same storage operations. `plasmite_feed`, `plasmite_fetch` and `plasmite_follow` cover messages, alongside `plasmite_pool_create`, `plasmite_pool_info`, `plasmite_pool_list` and `plasmite_pool_delete`. Pool resources use `plasmite:///pools/{name}` and include the pool description when one exists. `plasmite_pool_create` accepts `about` and `example`. Pool info and lists return both fields when present. On HTTPS, both protocol versions omit write tools from read-only grants and return `Permission` for any direct write call with such a grant.

Plasmite supports [MCP `2026-07-28`](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning) and [MCP `2025-11-25`](https://modelcontextprotocol.io/specification/2025-11-25) on both transports. With `2026-07-28`, clients can call `server/discover` or send a tool or resource request directly. Each request carries the protocol version and client capabilities in `params._meta`. Successful results include `resultType: "complete"` and server identity. Clients using `2025-11-25` start with `initialize` and `notifications/initialized`.

With `2026-07-28`, [Streamable HTTP](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http) requests mirror the version and method as `MCP-Protocol-Version` and `Mcp-Method`. Tool calls and resource reads also mirror the tool name or resource URI as `Mcp-Name`. The server rejects missing or mismatched headers. With `2025-11-25`, HTTP requests carry `MCP-Protocol-Version: 2025-11-25` after initialization, and the initialization notification returns HTTP 202. Both revisions use the same HTTPS access grants and require Client ID Metadata Documents for new sign-ins.

`plasmite_fetch` and `plasmite_follow` accept inclusive `since`, `last` and `tags`. Each call returns one batch. `count` defaults to 20 and accepts 1 through 200. `plasmite_follow` accepts `timeout_ms` from 0 through 60,000 (default 10,000). Each call examines at most 1,000 records, including records the tag filter rejects. Each successful result returns `next_seq`, the next unexamined sequence, and `ended`. A fetch that reaches its opening upper bound returns `complete`. Hitting a count, scan or wait limit returns `limit`. Continue by passing `next_seq` as `since` if it contains a sequence.

Retention loss returns a structured `RetentionGap` error with `missing` and `next_seq`. If the reader finds an error after collecting messages, it first returns the collected prefix with `ended: "limit"` and points `next_seq` at the first unread sequence. Resume there to check that sequence. Reading a pool resource returns the same result shape as a fetch of the last 20 messages.

### Events

On Streamable HTTP with `2026-07-28`, Plasmite advertises the beta [MCP Events extension](https://modelcontextprotocol.io/extensions/events/overview). `events/list` exposes `pool.message` with push delivery. Call `events/stream` with `Mcp-Name: pool.message`, one pool name in `arguments.pool`, and optional exact-match `arguments.tags`. A message must have every requested tag.

An event's `data` contains `pool` and `seq`, a decimal sequence string. Use `seq` as `since` with `plasmite_fetch` to read the message. An absent or null cursor starts at the live tail. A saved cursor replays retained messages and carries the pool incarnation so a recreated pool cannot reuse the old position. Active and heartbeat notifications carry the next position the server has checked. The stream signals skipped messages or a replaced pool on reconnect with `truncated: true`. Closing the response ends the subscription. Losing access ends the stream. Event streams share the live-read limit with other pool readers.
