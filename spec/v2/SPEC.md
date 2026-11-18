# Plasmite 2.0 contract

This contract covers the 2.0 CLI, libraries, HTTP, UDP and MCP. The corresponding 1.x specifications continue to describe 1.x. Plasmite 2.0 keeps the on-disk pool format and stored message envelope, so old pool files remain readable and writable without migration. The source APIs, native binary interface, HTTP protocol and CLI binary stream format make a major-version cutover.

## Common operations

- A client selects one local directory or remote server. The default local directory remains the home directory's `.plasmite/pools`. A bare name resolves within that directory. A path with a separator resolves as a file. An HTTPS pool reference selects a server and its saved connection. A UDP reference selects the datagram transport and the subset described under [UDP](#udp).
- `open_pool(ref)` returns a pool handle or `NotFound`. `open_pool(ref, create=true)` creates a missing pool at the existing default size and leaves an existing pool's size unchanged. `create_pool(ref, size)` returns a pool handle and reports `AlreadyExists` for an existing pool. Both work for local and HTTPS references.
- `pool.info()` and `list_pools()` describe the reference, file size, and first and last retained sequences. Empty pools have no bounds. Each result describes what the inspection saw and can go stale after it returns. Existing diagnostic fields may accompany the shared fields. `delete_pool` keeps the existing storage, permission and busy rules. Releasing a handle leaves the pool on disk.
- `feed(value, tags, durability)` accepts an object and stores it as data with the given tags. Non-object data produces `Usage`. A typed `Message` input forwards its data and tags. Supplying tags together with a `Message` produces `Usage` in every interface, matching the CLI.
- A feed assigns a new sequence and samples system wall time, then returns the committed `Message`. `fast` is the default durability. `flush` waits for storage to flush before a successful return. Writes never retry automatically. An I/O failure can arrive after the message is already in the pool.
- The envelope is `{seq,time,meta:{tags},data}`. Sequence starts at 1 and defines identity and order within a pool. Libraries expose one `time` field in Unix nanoseconds on messages, views, and encoded frames. Text uses UTC RFC 3339 with exactly nine fractional digits. Time can repeat or go backward. A forwarded message gets the destination's sequence and time.

## Errors

Every interface uses the same error kinds: `Usage`, `NotFound`, `AlreadyExists`, `Busy`, `Permission`, `Corrupt`, `Io`, `RetentionGap` and `Internal`. 2.0 adds no timeout kind; callers bound waits with their own cancellation. A `RetentionGap` carries `missing` and `next_seq` as described under [Reads](#reads).

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

The caller stops a read by leaving its loop, cancelling or closing the reader. Cancellation releases associated work and keeps messages already returned. The library read options leave counts, idle timeouts and gap policy to the caller. The CLI offers `follow --one` to wait for a single matching message. C, Python and JavaScript readers also offer a per-call wait for the next record, which can expire while the reader stays open. Python and JavaScript take the wait as a duration; C takes milliseconds in a field named for its unit. Go passes a context to each read instead. A checkpoint belongs to one pool: after completing sequence N, resume by opening at N+1, and discard checkpoints when a pool is replaced. Pool information is for inspection, not for advancing a checkpoint.

## Binding names and representations

| Operation | Python | JavaScript | Rust | Go | C |
| --- | --- | --- | --- | --- | --- |
| Create | `create_pool` | `createPool` | `create_pool` | `CreatePool` | `plsm_pool_create` |
| Open | `open_pool` | `openPool` | `open_pool` | `OpenPool` | `plsm_pool_open` |
| Info | `pool.info` | `pool.info` | `pool.info` | `pool.Info` | `plsm_pool_info` |
| List | `list_pools` | `listPools` | `list_pools` | `ListPools` | `plsm_pool_list` |
| Delete | `delete_pool` | `deletePool` | `delete_pool` | `DeletePool` | `plsm_pool_delete` |
| Store | `feed` | `feed` | `feed` | `Feed` | `plsm_pool_feed_json` and typed input variants |
| Live read | `follow` | `follow` | `follow` | `Follow` | `plsm_pool_follow` |
| Finite read | `fetch` | `fetch` | `fetch` | `Fetch` | `plsm_pool_fetch` |

Python and JavaScript provide default-directory module functions and explicit clients. Python opens reads synchronously; JavaScript resolves a Promise after the read opens. Python exposes `RetentionGap.missing` and `next_seq` and cancels through an event or by closing the reader. JavaScript uses `missing` and `nextSeq` and cancels through an `AbortSignal` or the async iterator's `return`. Both use `plasmite.at(t)` to mark a numeric nanosecond instant, because a bare number always means a sequence. Rust uses `Read` and `Feed` option builders, a typed instant selection, and an explicit cancellation flag or dropping the reader. Go uses option functions including `Since`, `Last`, `Tags` and `At`, with context cancellation. Rust and Go expose the gap fields with their native casing. C uses size-tagged option structs, an explicit discriminator for the kind of `since`, separate gap fields on the error, reader cancel and explicit free.

Optional fast-path suffixes are `_json`, `_lite3`, `_view` and the feed-only `_receipt`, with idiomatic casing. They combine in that order, and no binding has to offer every combination. Encoded input, view ownership and compact receipts stay distinct. View messages keep immutable backing storage under the binding's documented ownership, which lets them avoid conversion and copies across the language boundary. Local reads may still copy storage into an owned snapshot.

## Native library

The 2.0 C library carries its major version in its file name, so a program built for 1.x can't load it by accident:

| Platform | 1.x | 2.0 |
| --- | --- | --- |
| Linux | `libplasmite.so` (soname `libplasmite.so`) | `libplasmite.so.2` (soname `libplasmite.so.2`) |
| macOS | `libplasmite.dylib` (install name `@rpath/libplasmite.dylib`) | `libplasmite.2.dylib` (install name `@rpath/libplasmite.2.dylib`) |
| Windows | `plasmite.dll` | `plasmite2.dll`, with import library `plasmite2.lib` |

The header defines `PLSM_ABI_MAJOR 2`, and the library exports `uint32_t plsm_abi_major(void)`, which returns 2. Python and Go load the versioned file. The npm package places that same versioned library next to the Node add-on, which links dynamically against its versioned name. All three check `plsm_abi_major()` at load before using other Plasmite calls. A mismatch fails with a clear error. The versioned filenames allow 1.x and 2.0 libraries to coexist.

## CLI

- `feed POOL [OBJECT]`, `follow POOL` and `fetch POOL [SEQ]` use the shared operations. `--create` asks `feed`, `follow` or `duplex` to create a missing pool at the default size, for local and HTTPS pools. `pool create --size SIZE` sets a custom size. `pool create`, `info`, `list` and `delete` remain available.
- `--since` accepts a decimal sequence, `oldest`, RFC 3339 or a relative duration. `follow` and `fetch` also accept `--last N` or `-n N`, and a repeatable `--tag TAG`.
- `follow --one` exits successfully after writing the first matching message on every platform. With the default start, it waits for a new message. On Linux and macOS, local follow also exits successfully when its downstream command closes the pipe, even while the pool is quiet. Use `--one` for scripts that must work across shells, including PowerShell pipelines that keep the pipe open after taking one message.
- `fetch POOL SEQ` requests exactly one sequence. It prints that record, reports `RetentionGap` if the record was dropped, reports `NotFound` if SEQ is the next unwritten sequence, and reports `Usage` if SEQ is beyond it. The library equivalent takes the first item of `fetch(since=SEQ)`, where an empty reader means SEQ isn't written yet.
- `feed` accepts a positional object or stdin. Text whose shape exactly matches a valid `{seq,time,meta:{tags},data}` envelope is forwarded as a message. Other objects are application data. The source `seq` must be positive and the source `time` may use any valid RFC 3339 form. A malformed reserved envelope fails with `Usage`, and `--tag` with a forwarded message produces `Usage`. Existing JSON, YAML and Lite3 recognition and explicit format choices remain.
- `--input-format lines` stores one `{"line":"..."}` object per input line. A line ends at `\n`; a trailing `\r` is removed. A final line without a newline is stored. Input that isn't valid UTF-8 fails with `Usage` at the offending line, after the earlier lines are stored.
- Normal output keeps complete messages. Output defaults remain readable text in a terminal and JSON in a pipe. Diagnostics go to stderr. `--format lite3` writes the binary stream described under [Binary stream](#binary-stream), without control records; the exit status reports how the read ended.
- 2.0 replaces `--tail` with `--last` and `follow --no-follow` with `fetch`. It removes `--where`, `--timeout`, `--replay`, `--quiet-drops`, `--no-notify`, `--file`, `--retry`, `--retry-delay` and `--create-size`. It retires `tap`, including its separate stream labels, lifecycle records and signal forwarding. `duplex` still opens its receiver before sending and uses the shared feed and follow options where they apply.

## HTTP

The 2.0 server and 2.0 clients use a separate `/v2` API. `/v2/pools` is the pool collection and `/v2/pools/{pool}` names a pool.

- POST on the collection creates a pool and returns its information, which the client uses to construct a handle. GET on the collection lists pools. GET and DELETE on a pool inspect and delete it. POST `/v2/pools/{pool}/open` checks that the pool exists, or creates it when the request asks.
- POST `/v2/pools/{pool}/feed` stores a message.
- GET `/v2/pools/{pool}/follow` and `/v2/pools/{pool}/fetch` read, with `since`, `last` and repeated `tag` query parameters. `follow` resolves its opening position before the successful response reaches the caller.
- Existing limits on access, request sizes, server resources and credentials remain.

Server and clients upgrade together. A 2.0 server answers 1.x routes with an error that names the required protocol version. A 2.0 client that receives 404 from a `/v2` route reports that the server runs 1.x and needs upgrading.

### Stream endings

Every read response ends with exactly one control record:

| Type | Meaning | Client library behavior |
| --- | --- | --- |
| `complete` | A fetch reached its upper bound. | Ends the reader normally. |
| `error` | The read failed. Carries the standard structured error, such as `RetentionGap` with `missing` and `next_seq`. | Raises the error. |
| `cutoff` | A server limit ended the response. Carries `next_seq`, the next unexamined sequence, and for a fetch `until`, its original upper bound. | Sends a new request at exactly `next_seq`, adding `until` for a fetch, and continues the reader. |

The server sends every matching record before it advances past it, so a cutoff never skips one. The next request uses the cutoff's `next_seq` even when the response carried no matching records. A continued fetch keeps its original upper bound instead of taking a new snapshot. `until` is a protocol parameter for continuation only; libraries don't expose it as a read option. A follow on a quiet pool may see repeated cutoffs without new messages. A cutoff never hides a retention gap or turns incomplete history into a successful completion. A response that ends without a control record raises `Io`, unless the caller cancelled or closed the reader.

Each successful read response carries `Plasmite-Incarnation`, the opaque identity of the opened pool. A client checks that the identity still matches before accepting records from a continuation. A different identity ends the read with `NotFound`. A continuation with a missing identity raises `Io`.

Fetching one record over HTTP opens a fetch and closes it after the first record. Closing a response early stops the server's work for that read promptly.

### JSON stream

A JSON stream carries one object per line. Message records are envelopes. The control record is an object with a top-level `type` of `complete`, `error` or `cutoff` and the fields in the table above. No envelope has a top-level `type`, so the two can't be confused.

### Binary stream

A binary stream is a sequence of records, each starting with a one-byte kind and a four-byte big-endian body length:

| Kind | Body |
| --- | --- |
| `0x4D` (`M`), message | `[u64be seq][u64be time_ns][payload]`, where the payload is the stored Lite3 message |
| `0x43` (`C`), control | UTF-8 JSON with the same fields as the JSON control record |

The HTTP binary stream uses these records directly. The CLI's `--format lite3` output starts with the four ASCII bytes `PLS2` and then carries message records only. `feed --input-format lite3` reads the same format and rejects control records.

## UDP

`feed` submits a datagram and returns no stored receipt. `follow` and `fetch` accept every selection in [Reads](#reads): omitted, `oldest`, a sequence, a time, and `last`. A UDP `fetch` is a finite subscription that ends at the server's terminal progress. UDP doesn't offer pool lifecycle or inspection. Reads deliver received messages in arrival order, including duplicates and late messages, alongside the server's skip and progress events. A server-announced retention gap ends the subscription and raises `RetentionGap` once with `missing` and `next_seq`. The server repeats the terminal error on renewal until the lease ends or the client closes. A datagram the network drops produces no error, so a completed UDP fetch doesn't prove the history is complete. UDP keeps its documented wire limits, skip reasons, leases and receive policy. Unsupported operations fail before sending.

## MCP

Local stdio and direct HTTPS register the same tool schemas and return the same outcomes. `plasmite_feed`, `plasmite_fetch` and `plasmite_follow` cover the storage operations, alongside the existing pool tools. The message envelope, runtime authorization and existing resource paths stay the same.

`plasmite_fetch` and `plasmite_follow` accept inclusive `since`, `last` and `tags`. Because each tool call returns one response, both keep a bounded record count, and `plasmite_follow` keeps a bounded wait. Every result returns `next_seq`, the next unexamined sequence including filtered-out records, and `ended`, which says why the batch stopped: `complete` when a fetch reached its upper bound and `limit` when the count or wait ran out. A caller continues by passing `next_seq` as `since`. Retention loss returns a structured `RetentionGap` error with `missing` and `next_seq`, not a success carrying `fell_behind`. If a read finds an error after collecting messages, it returns the collected prefix with `ended: "limit"` and points `next_seq` at the first unread sequence. Resuming there reports the error. The server also bounds how many records one call examines, including records that the tag filter rejects. Reading a pool resource returns the same result shape as a fetch of the last 20 messages. The names `after_seq` and `next_after_seq` aren't reused.
