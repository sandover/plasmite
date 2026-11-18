# Upgrade to Plasmite 2.0

Plasmite 2.0 opens existing pool files in place. Upgrade HTTP clients and servers together, rebuild against the C library if you link it, and update the calls and commands below.

## Calls and commands

| 1.x | 2.0 |
| --- | --- |
| Binary HTTP follow can't report a final error | It ends with an error record |
| Bindings: `time` is a microsecond `datetime` in Python, a millisecond `Date` or a string in JavaScript and a `time.Time` in Go, beside a separate nanosecond field | `time` is integer nanoseconds in every binding |
| Bindings: `append`, `send_json` | `feed`, including for UDP |
| Bindings: `get(seq)` | The first message of `fetch(since=seq)` |
| Bindings: `follow(finite=True)` | `fetch` |
| Bindings: several start options, such as a sequence start and `since_ns` | `since` or `last` |
| Bindings: `timeout_ms`, `idleTimeoutMs`, `max_messages` | The caller cancels or leaves the loop |
| Bindings: open-or-create helpers | `open_pool(ref, create=True)` |
| Bindings: Rust `create_pool` returns `PoolInfo` | `create_pool` returns a pool handle |
| Bindings: lost messages are skipped by default, with an option to fail | Local and HTTP reads report every loss as `RetentionGap` |
| CLI: `follow --no-follow` | `fetch` |
| CLI: `--tail N` | `--last N` (`-n` stays) |
| CLI: `fetch POOL SEQ` for one message | `fetch POOL SEQ` for one message, or `follow POOL --since SEQ` to resume and keep listening |
| CLI: `--timeout` | Close stdout, stop the process, or use your process manager to enforce a time limit |
| CLI: `follow --one` | Wait for one matching message and exit on every platform |
| CLI: `--where EXPR` | `jq 'select(EXPR)'` |
| CLI: `-f/--file FILE` | `< FILE` |
| CLI: `--create-size SIZE` | `--create` at the default size, or `pool create --size SIZE` first |
| CLI: `--retry`, `--retry-delay` | The caller retries |
| CLI: `tap -- CMD` | A pipe into `feed --input-format lines`; see [Capture command output](../cookbook.md#capture-command-output) |
| CLI: `--quiet-drops`, `--no-notify`, `--replay` | Removed |

In Bash, use `set -o pipefail` when a captured command's failure should fail the pipeline. Rebuild C programs against the 2.0 header. The new shared library names are `libplasmite.so.2` on Linux, `libplasmite.2.dylib` on macOS, and `plasmite2.dll` on Windows. Python, Go and the Node add-on check the library's major version when they load it.

## Resume Brygg's shared canvas listener

Brygg's app draws a shared canvas from tagged messages. It supplies the callback and two checkpoint functions. On the first run, `load_last_handled()` returns `None`, so `follow` starts with new messages. After a restart, the listener asks for the message after the last one it handled.

```python
import plasmite

pool = plasmite.open_pool("grObjPool")
print(pool.info())  # Shows the oldest and newest sequences still in the pool.

last_seq = load_last_handled()
since = None if last_seq is None else last_seq + 1
for message in pool.follow(since=since, tags=["gfx", "scope:sharedCanvas"]):
    handle_message(message)
    save_last_handled(message.seq)
```

If the app last handled 100 and `pool.info()` shows 90 through 120, the listener starts at 101, catches up, and keeps waiting for new messages. It saves each sequence only after the callback succeeds.

If 101 has already expired, `follow` raises `RetentionGap` before delivering anything. The pool knows which sequence numbers it lost, but not whether any of them carried canvas tags, so the app can no longer verify that its canvas is complete. It needs a known state, such as a fresh snapshot from wherever the canvas is saved, before it applies more updates.

Keep a checkpoint with the pool it came from. Discard it if you replace the pool, because the new pool assigns its own sequences starting at 1.

## Keep encoded data with owned views

The new view APIs keep one immutable Lite3 snapshot instead of decoding an entire message into language containers. Use them for selected reads, retained messages, typed decoding and forwarding. Install matching bindings and native libraries before using the new symbols.

| Language | Produce / read | Local stream | Explicit native copy |
|---|---|---|---|
| [Rust](../../README-crate.md#immutable-snapshots-and-typed-values) | `feed_view` / `fetch_view` | `follow_view` | `value.decode::<T>()` when `T` owns its fields |
| [Python](../../bindings/python/README.md#immutable-message-views) | `feed_view` / `fetch_view` | `follow_view` | `to_native(value)` |
| [JavaScript](../../bindings/node/README.md#owned-views) | `feedView` / `fetchView` | `followView` | `toNative(value)` |
| [Go](../../bindings/go/README.md#managed-owned-views) | `FeedView` / `FetchView` | `FollowView` | `Decode(&target)` |
| [C](../../include/plasmite.h) | Native builder / `plsm_pool_fetch` with `plsm_view_stream_next` | `plsm_pool_follow` with `plsm_view_stream_next` | Caller-selected scalar/span copies |

Field access reads the snapshot. It may allocate language scalars or small wrappers, but does not build the rest of the native tree. Acquisition copies pool or transport data into independent storage and validates it once. Closing a pool or stream, or overwriting a ring slot, cannot invalidate a returned view. A small retained child keeps the complete message alive. Convert only that subtree when you want to release the larger snapshot.

Rust borrows values from `MessageView`; `ValueRef::into_owned()` retains a child by sharing its snapshot. C callers retain/release the message owner and keep it alive while using borrowed value references. Python and JavaScript children retain their owners automatically. Go views and children retain immutable Go-managed buffers and need no per-message `Close`. Streams, follows and Go prepared selectors still own resources that callers must close.

Readonly buffers do not grant mutable access to the stored snapshot. Go `Lite3()` and JavaScript `toLite3(message)` return independent mutable copies. Python `memoryview(message)` and Rust `message.bytes()` borrow readonly payload bytes. These payloads contain stored metadata and data; sequence and timestamp identity travel separately. Timestamps count Unix nanoseconds. The text envelope uses exactly nine fractional digits. Whole-message JSON/native exports use the canonical `seq`, `time`, `meta.tags`, `data` envelope; subtree exports preserve their stored fields, including additional metadata.

Forward a whole view to preserve its encoding and tags while assigning a new destination identity. Use the binding's feed view method with tags omitted. To supply new tags, feed the message's data. Feeding a data subtree copies it into a new encoding.

### Exact numbers and native bytes

New view writers accept signed 64-bit integers and finite binary64 floats. They reject an out-of-range integer before publication. JSON integer tokens also must fit i64; a decimal point or exponent requests a float. Legacy writers retain their existing conversion rules. Native byte values stay bytes; JSON exports represent them as base64 strings.

Python reads i64 as `int`. JavaScript reads safe i64 as `number` and larger i64 as `bigint`; stored floats stay `number`, including negative zero. Use `bigint` to submit an exact large integer: converting it to `number` first already loses precision. JavaScript sequence and nanosecond timestamp fields always use `bigint`. Use `toJson` for exact JSON integer digits; `JSON.stringify` rejects bigints. Rust and Go typed decoding report incompatible target types or ranges. New object keys cannot contain NUL. Consult each binding guide for supported input types and serializer hooks; the Python native writer accepts exact builtins rather than invoking custom object serializers.

### Streams and transport limits

Owned local streams share existing tag, gap and sequence rules. They do not implicitly prefetch or batch. Python's `next_batch` requests explicit count and encoded-byte bounds. Cancel a stream when abandoning a blocked read, and close its resource handle. Node async iterators close on `break`; Python context managers and Go `defer Close()` provide the corresponding cleanup. Cancellation interrupts message waits; it cannot interrupt a file lock already in progress. In Node pass a duration to `reader.next`. In Go pass a context to `Next` to set a deadline.

UDP streams deliver messages in arrival order, including duplicates and late packets. They report losses the server announces. A network drop can go unnoticed, and a finite UDP read can end with messages still in flight.

JavaScript's HTTP client offers `fetchView`, `followView` and `feedView` through its bundled WebAssembly backend. Both JSON and binary HTTP reads report failures in a final record. See the [HTTP contract](../../spec/v2/SPEC.md#http) for how clients recognize a successful read and continue across server limits.

## JSON reads preserve stored floating-point values

Rust now enables correctly rounded JSON float parsing. C JSON fetch and follow format stored values directly and preserve the stored binary64 bits.

The earlier Rust parser could round a value again during a read. For example, it could turn stored `2.027716612057172e155` into `2.0277166120571721e155`. Applications that compare exact float values or saved JSON text can observe the correction. Regenerate expectations from the stored value when a fixture captured that extra rounding. The pool format and its signed-integer/binary64 payload range stay the same; sequence and timestamp identity keep their exact integers.

## Duplex sends and receives ordinary messages

`duplex` uses the same input parser as `feed`, including at a terminal. Enter JSON objects, complete messages, marked YAML, or framed Lite3. Duplex delivers every matching message, including its own appends. Put any application-specific identity in `data` or tags.

When forwarding a complete text message, use a positive source sequence and any valid RFC 3339 timestamp. The receiving pool assigns the stored message its own sequence and time and preserves the source tags and data.

`pls duplex chat` opens its reader before sending its first message. Input EOF closes sending, so a program can send a request, close stdin, and still read a reply. Stop the process or close stdout when reception is done. Use `feed` for a finite copy that exits at input EOF.

## Migrate MCP clients

Replace `plasmite_read` with `plasmite_fetch` and `plasmite_wait` with `plasmite_follow`. Where 1.x passed `after_seq=N`, pass `since=N+1`. Continue a batch by passing its `next_seq` directly as `since`. `ended` tells you whether the fetch reached its opening end or whether the call hit a limit. A `RetentionGap` error carries `missing` and `next_seq`. Readers that accept incomplete history can resume at the sequence in the error. The 1.x result fields `next_after_seq` and `fell_behind` are gone.

## Upgrade HTTP clients and servers together

| 1.x route | 2.0 route |
| --- | --- |
| `POST /v0/pools/{p}/append` | `POST /v2/pools/{p}/feed` |
| `GET /v0/pools/{p}/messages/{seq}` | First record of `GET /v2/pools/{p}/fetch?since=SEQ` |
| `GET /v0/pools/{p}/follow` | `GET /v2/pools/{p}/follow` or `GET /v2/pools/{p}/fetch` |
| `GET /v0/pools/{p}/info` | `GET /v2/pools/{p}` |
| `POST /v0/pools/open` | `POST /v2/pools/{p}/open` |

A 2.0 read ends with a control record. `complete` ends a finite read. `error` carries the failure and any retention fields. `cutoff` tells the client to continue at `next_seq`, keeping the original `until` on a fetch. Read the [HTTP contract](../../spec/v2/SPEC.md#http) before updating a client.

## Rebuild C callers

| 1.x C call | 2.0 C call |
| --- | --- |
| `plsm_pool_append_*` | `plsm_pool_feed_*` |
| `plsm_pool_get_*` | Open `plsm_pool_fetch` or `plsm_pool_fetch_lite3` at the sequence and take the first record |
| `plsm_stream_open(_ex)`, `plsm_lite3_stream_open(_ex)` | Open `plsm_pool_follow`, `plsm_pool_fetch`, or their `_lite3` forms with `plsm_read_options_t` |
| `plsm_pool_follow_views` | Open `plsm_pool_follow` and pull views with `plsm_view_stream_next` |
| `plsm_udp_pool_send_*` | `plsm_udp_pool_feed_*` |
| `plsm_udp_pool_follow` | `plsm_udp_pool_read` |

The Node feed family returns promises, including JSON and Lite3 receipt methods. Go feed variants take `FeedOption` values such as `WithDurability`. Python and JavaScript UDP waits now use the same units as other readers.

## Convert framed Lite3 streams

The CLI reads and writes `PLS2` framed streams. To copy a `PLS1` stream into 2.0, read it with 1.x and feed the resulting JSON messages into 2.0. Existing pool files open directly.

## Applications control playback timing

Read history with `fetch` and use your language's timer to pace each message. For example, this Python loop processes retained messages ten times faster than their original timing.

```python
import time

previous = None
with pool.fetch() as reader:
    for message in reader:
        if previous is not None:
            time.sleep(max(0, message.time - previous) / 10_000_000_000)
        handle(message)
        previous = message.time
```

A clock step can make the next message's timestamp smaller. This example continues immediately when that happens. It keeps the pool's sequence order.

## Default pools stay in the user's home directory

The default remains `~/.plasmite/pools` (`%USERPROFILE%\.plasmite\pools` on Windows). Every client now follows the same home policy. Unix clients use an absolute, nonempty `HOME`, then the current account's home. Windows clients use an absolute `USERPROFILE`, then the platform's profile directory. Empty or relative environment values do not select a relative directory. If no home can be resolved, supply `--dir PATH` or an explicit client directory. Explicit paths can remain relative. Installed services keep their configured directories.

No migration is necessary for pools already in the home directory. Earlier Rust clients with missing `HOME` could create `.plasmite/pools` beneath their working directory; Go clients could select a temporary `plasmite/pools` directory. Inspect those locations before upgrading. Select an old location explicitly, or stop its writers and move the pool files into your chosen directory. Plasmite does not move files or search alternate stores automatically. Earlier Windows Rust clients followed `HOME`; if it differs from `USERPROFILE`, select the old `HOME/.plasmite/pools` location explicitly. If you used a relative home value, inspect the corresponding directory beneath the old working directory too.

Rust's `LocalClient::pool_dir()` now returns `Result<&Path, Error>`; propagate or handle its missing-home error. `LocalClient::new()` remains an infallible configuration builder, and `with_pool_dir` and explicit `PoolRef::Path` operations work without a default home. Go's `local.DefaultPoolDir()` now returns `(string, error)`; `NewDefaultClient()` propagates that error. Node's `DEFAULT_POOL_DIR` and Python's equivalent are unavailable (`undefined`/`None`) when home resolution fails; explicit clients remain usable.

## Node error sequence and offset fields use bigint

Both `PlasmiteNativeError` and `RemoteError` now expose optional `seq` and `offset` fields as `bigint`. This preserves every unsigned 64-bit value, including IDs above JavaScript's safe-integer limit. Absence remains `undefined`, and zero remains `0n`. Compare with bigint literals and convert to decimal strings when serializing error context to JSON:

```javascript
if (error.seq !== undefined) {
  console.error(`Message ${error.seq.toString()} failed`);
}
```

## Explicit local paths through string bindings

C, Node, and Python distinguish names from paths by native path separators. Use `./work.plasmite` (or `.\work.plasmite` on Windows) for a bare file in the working directory; `work.plasmite` selects that name in the client's pool directory. Absolute Windows drive paths, UNC paths, and backslash-relative paths now reach the local file API. Drive-relative forms such as `C:work.plasmite` remain unsupported. Go's `PoolRefPath("work.plasmite")` now preserves explicit path intent by adding the relative prefix. `PoolRefName` and raw string names retain their meaning. POSIX names can still contain a literal backslash.

## Numeric feed receipts

Use a receipt when a producer needs the committed sequence and time. A consumer can still read the complete message. The receipt carries no data or tags. Its `time` counts Unix nanoseconds.

| Interface | Receipt operation |
| --- | --- |
| Rust | `feed_lite3_receipt` |
| C | `plsm_pool_feed_json_receipt`, `plsm_pool_feed_lite3_receipt` |
| Python | `feed_receipt`, `feed_json_receipt`, `feed_lite3_receipt` |
| JavaScript | `feedReceipt`, `feedJsonReceipt`, `feedLite3Receipt` |
| Go | `FeedReceipt`, `FeedJSONReceipt`, `FeedLite3Receipt` |
