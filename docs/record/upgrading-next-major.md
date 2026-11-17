# Upgrade to the next major release

These changes exist in the source checkout and await a major release. Package
versions still identify the last release; this guide does not announce a release.
See the [changelog](../../CHANGELOG.md#unreleased) for the UDP delivery changes.

## Keep encoded data with owned views

The new view APIs keep one immutable Lite3 snapshot instead of decoding an
entire message into language containers. Use them for selected reads, retained
messages, typed decoding and forwarding. Existing message APIs keep their
published return types and behavior; applications can migrate one call at a time.
Install matching bindings and native libraries before using the new symbols.

| Language | Produce / read | Local stream | Explicit native copy |
|---|---|---|---|
| [Rust](../../README-crate.md#immutable-snapshots-and-typed-values) | `append_value_now` / `get_view` | `follow_views` | `value.decode::<T>()` when `T` owns its fields |
| [Python](../../bindings/python/README.md#immutable-message-views) | `append_view` / `get_view` | `follow_views` | `to_native(value)` |
| [JavaScript](../../bindings/node/README.md#owned-views-additive-source-api) | `appendView` / `await getView` | `followViews` | `toNative(value)` |
| [Go](../../bindings/go/README.md#managed-owned-views) | `AppendView` / `GetView` | `OpenViewFollow` or `FollowViews` | `Decode(&target)` |
| [C](../../include/plasmite.h) | native builder or `plsm_pool_append_view_json` / `plsm_pool_get_view` | `plsm_view_stream_open` | caller-selected scalar/span copies |

Field access reads the snapshot. It may allocate language scalars or small
wrappers, but does not build the rest of the native tree. Acquisition copies
pool or transport data into independent storage and validates it once. Closing
a pool or stream, or overwriting a ring slot, cannot invalidate a returned view.
A small retained child keeps the complete message alive. Convert only that
subtree when you want to release the larger snapshot.

Rust borrows values from `MessageView`; `ValueRef::into_owned()` retains a child
by sharing its snapshot. C callers retain/release the message owner and keep it
alive while using borrowed value references. Python and JavaScript children
retain their owners automatically. Go views and children retain immutable
Go-managed buffers and need no per-message `Close`. Streams, follows and Go
prepared selectors still own resources that callers must close.

Readonly buffers do not grant mutable access to the stored snapshot. Go
`Lite3()` and JavaScript `toLite3(message)` return independent mutable copies.
Python `memoryview(message)` and Rust `message.bytes()` borrow readonly payload
bytes. These payloads contain stored metadata and data; sequence and timestamp
identity travel separately. Timestamp identity stays exact in nanoseconds;
language time objects arise on request. Python datetime has microsecond
precision, while its `time_rfc3339` preserves nanoseconds.
Whole-message JSON/native exports use the canonical
`seq`, `time`, `meta.tags`, `data` envelope; subtree exports preserve their stored
fields, including additional metadata.

Forward a whole view to preserve its encoding and tags while assigning a new
destination identity. Rust uses `append_view_now`; Python and JavaScript use
`append_view(message)` / `appendView(message)` with tags omitted; Go uses
`AppendView(message, nil)`. Explicit tags rebuild the envelope around the data
and replace the tags; an empty tag list clears them. Forwarding a data subtree
copies it into a new encoding. Destination storage still writes the message.

### Exact numbers and native bytes

New view writers accept signed 64-bit integers and finite binary64 floats.
They reject an out-of-range integer before publication. JSON integer tokens
also must fit i64; a decimal point or exponent requests a float. Legacy writers
retain their existing conversion rules. Native byte values stay bytes; JSON
exports represent them as base64 strings.

Python reads i64 as `int`. JavaScript reads safe i64 as `number` and larger i64
as `bigint`; stored floats stay `number`, including negative zero. Use `bigint`
to submit an exact large integer: converting it to `number` first already loses
precision. JavaScript sequence and nanosecond timestamp fields always use
`bigint`. Use `toJson` for exact JSON integer digits; `JSON.stringify` rejects
bigints. Rust and Go typed decoding report incompatible target types or ranges.
New object keys cannot contain NUL. Consult each binding guide for supported
input types and serializer hooks; the Python native writer accepts exact
builtins rather than invoking custom object serializers.

### Streams and transport limits

Owned local streams share existing tag, gap and sequence rules. They do not
implicitly prefetch or batch. Python's `next_batch` requests explicit count and
encoded-byte bounds; Go's channel adapter accepts an explicit buffer. Cancel a
stream when abandoning a blocked read, and close its resource handle. Node
async iterators close on `break`; Python context managers and Go `defer Close()`
provide the corresponding cleanup. Cancellation interrupts message waits;
it cannot interrupt a file lock already in progress. Node local tails use
`timeoutMs` for each wait. Go tail polling waits use `FollowOptions.Timeout`;
use the context for a deadline.

Owned UDP follows preserve arrival order, duplicates and late packets. Explicit
server skips produce gaps; sequence jumps alone do not establish packet loss.
A finite follow can end with DATA still in flight. Views do not add delivery
acknowledgments or repair to UDP.

Node's HTTP `RemotePool` provides `getView`, `followViews` and `appendView` through
the bundled WebAssembly backend, without a native addon. Binary HTTP reads need
current-server identity headers. Binary tails reject `errorOnGap: true`: their
frame protocol cannot report a terminal error after streaming begins. EOF does
not certify complete delivery. Use the JSON tail when you require its
fail-closed gap reporting. Other languages' local view APIs do not imply HTTP
support. See the binding guides for examples and the
[measurement index](../performance/language-boundary-results/owned-views/README.md)
for workloads where views help or add cost.

## JSON reads preserve stored floating-point values

Rust now enables correctly rounded JSON float parsing. The C JSON get/tail path
formats stored values directly instead of exporting JSON, parsing a Rust object
tree and serializing it again. Both routes preserve the stored binary64 bits.

The earlier Rust parser could round a value again during a read. For example,
it could turn stored `2.027716612057172e155` into `2.0277166120571721e155`.
Applications that compare exact float values or saved JSON text can observe the
correction. Regenerate expectations from the stored value when a fixture captured
that extra rounding. The pool format and its signed-integer/binary64 payload
range stay the same; sequence and timestamp identity keep their exact integers.

## Duplex sends and receives ordinary messages

`duplex` uses the same input parser as `feed`, including at a terminal. Enter
JSON objects, complete messages, marked YAML, or framed Lite3. It preserves the
existing `seq`, `time`, `meta.tags`, and `data` envelope and assigns no meaning
to application fields inside `data`. It delivers its own appends too.

Remove `--me` and `--echo-self`. Applications can choose their own data and tags.
For example, send application-defined chat data explicitly:

```sh
printf '{"from":"alice","msg":"hello"}\n' | pls duplex chat --timeout 5s || [ $? -eq 124 ]
```

Input EOF closes sending while reception continues. A program can send a request,
close stdin, and still read a reply. Choose a receive stop condition: Ctrl+C,
closed stdout, or `--timeout`, which exits 124 after the specified period without
output. Use `feed` for finite copying that exits when input ends. Duplex chooses
its receive starting position before sending its first message.

## HTTP tail durations are upper bounds

The server uses the shorter of the requested `timeout_ms` and its configured
maximum; omission uses the maximum. Admission, storage work, and backpressure
share that absolute lifetime. CLI `--timeout` still measures time without output.

A request above the maximum now succeeds with a shorter stream instead of
returning 400. Existing clients can consume these shortened streams. The new CLI
no longer probes an older server's maximum through errors. Upgrade the server
with the CLI; a new CLI talking to an older server can fail when its requested
wait exceeds that server's configured maximum. Wire encodings stay the same.

## SDK applications control playback timing

Timed playback remains available through `pls follow POOL --since 1h --replay 10`.
The SDKs expose message reading and let the application choose when to act on a
message. Remove these APIs from existing callers:

| SDK | Removed API |
| --- | --- |
| Rust | `Replay`, `ReplayOptions`, `PoolApiExt::replay` |
| Go | `ReplayOptions`, `Pool.Replay`, the `Replay` method on `api.Pool` |
| Python | `Pool.replay` |
| Node | `ReplayOptions`, `Pool.replay`, the module-level `replay` helper |

The C stream ABI keeps its existing operations and layouts.

The examples below read at most two messages from sequence 1 and apply 10x
playback timing. They use ordinary streams, which can include new appends.
Retention can remove unread messages; `error_on_gap`/`errorOnGap`/`ErrorOnGap`
requests an error when that happens. To protect a selected dataset from later
overwrite, collect or export it before playback. Collection does not provide an
atomic snapshot while other processes write. A read timeout does not cancel a
sleep the application starts afterward; use the host language's cancellation
mechanism when playback needs a deadline.

Rust applications can pace owned messages between reads:

```rust
use plasmite::api::{Error, GapPolicy, Pool, PoolApiExt, FollowOptions};
use std::time::Duration;

fn playback(pool: &Pool) -> Result<(), Error> {
    let mut tail = pool.follow(FollowOptions {
        start: plasmite::api::FollowStart::Sequence(1), max_messages: Some(2),
        timeout: Some(Duration::from_secs(1)), gap_policy: GapPolicy::Error,
        ..FollowOptions::default()
    });
    let mut previous = None::<u64>;
    while let Some(message) = tail.next_message()? {
        let timestamp = message.timestamp_ns()?;
        if let Some(prior) = previous {
            std::thread::sleep(Duration::from_nanos(timestamp.saturating_sub(prior) / 10));
        }
        println!("{}", message.data);
        previous = Some(timestamp);
    }
    Ok(())
}
```

Python applications can pause between generator reads without holding a pool lock:

```python
import time

previous = None
for message in pool.follow(start=FollowStart.sequence(1), max_messages=2, timeout_ms=1000, error_on_gap=True):
    if previous is not None:
        time.sleep(max(0, (message.time - previous).total_seconds() / 10))
    print(message.data)
    previous = message.time
```

Node applications should use the asynchronous tail and timer so other work keeps
running during reads and delays:

```javascript
const { setTimeout: sleep } = require("node:timers/promises");

let previous = null;
for await (const message of pool.follow({
  start: FollowStart.sequence(1n), maxMessages: 2, timeoutMs: 1000, errorOnGap: true,
})) {
  const timestamp = message.time.getTime();
  if (previous !== null) await sleep(Math.max(0, (timestamp - previous) / 10));
  console.log(message.data);
  previous = timestamp;
}
```

Go applications can use a context to stop playback delays as well as reads:

```go
func playback(ctx context.Context, pool api.Pool) error {
    start, count := uint64(1), uint64(2)
    messages, errs := pool.Follow(ctx, api.FollowOptions{
        Start: local.Sequence(start), MaxMessages: &count, Timeout: time.Second, ErrorOnGap: true,
    })
    var previous time.Time
    for message := range messages {
        if !previous.IsZero() {
            timer := time.NewTimer(max(0, message.Time.Sub(previous) / 10))
            select {
            case <-timer.C:
            case <-ctx.Done():
                timer.Stop()
                return ctx.Err()
            }
        }
        fmt.Printf("%s\n", message.Data)
        previous = message.Time
    }
    return <-errs
}
```

The Go function uses `context`, `fmt`, `time`, and
`github.com/sandover/plasmite/bindings/go/api` imports. Give it a cancellable
context and cancel that context when the caller stops consuming.

## Default pools stay in the user's home directory

The default remains `~/.plasmite/pools` (`%USERPROFILE%\.plasmite\pools` on
Windows). Every client now follows the same home policy. Unix clients use an
absolute, nonempty `HOME`, then the current account's home. Windows clients use
an absolute `USERPROFILE`, then the platform's profile directory. Empty or
relative environment values do not select a relative directory. If no home can
be resolved, supply `--dir PATH` or an explicit client directory. Explicit paths
can remain relative. Installed services keep their configured directories.

No migration is necessary for pools already in the home directory. Earlier Rust
clients with missing `HOME` could create `.plasmite/pools` beneath their working
directory; Go clients could select a temporary `plasmite/pools` directory.
Inspect those locations before upgrading. Select an old location explicitly,
or stop its writers and move the pool files into your chosen directory. Plasmite
does not move files or search alternate stores automatically. Earlier Windows
Rust clients followed `HOME`; if it differs from `USERPROFILE`, select the old
`HOME/.plasmite/pools` location explicitly. If you used a relative home value,
inspect the corresponding directory beneath the old working directory too.

Rust's `LocalClient::pool_dir()` now returns `Result<&Path, Error>`; propagate
or handle its missing-home error. `LocalClient::new()` remains an infallible
configuration builder, and `with_pool_dir` and explicit `PoolRef::Path` operations
work without a default home. Go's `local.DefaultPoolDir()` now returns
`(string, error)`; `NewDefaultClient()` propagates that error. Node's
`DEFAULT_POOL_DIR` and Python's equivalent are unavailable (`undefined`/`None`)
when home resolution fails; explicit clients remain usable.

## Node error sequence and offset fields use bigint

Both `PlasmiteNativeError` and `RemoteError` now expose optional `seq` and
`offset` fields as `bigint`. This preserves every unsigned 64-bit value, including
IDs above JavaScript's safe-integer limit. Absence remains `undefined`, and zero
remains `0n`. Compare with bigint literals and convert to decimal strings when
serializing error context to JSON:

```javascript
if (error.seq !== undefined) {
  console.error(`Message ${error.seq.toString()} failed`);
}
```

## Explicit local paths through string bindings

C, Node, and Python distinguish names from paths by native path separators.
Use `./work.plasmite` (or `.\work.plasmite` on Windows) for a bare file in the
working directory; `work.plasmite` selects that name in the client's pool
directory. Absolute Windows drive paths, UNC paths, and backslash-relative paths
now reach the local file API. Drive-relative forms such as `C:work.plasmite`
remain unsupported. Go's `PoolRefPath("work.plasmite")` now preserves explicit
path intent by adding the relative prefix. `PoolRefName` and raw string names
retain their meaning. POSIX names can still contain a literal backslash.

## Numeric append receipts

Local producers can request only the committed sequence and Unix timestamp in
nanoseconds. Use `append_json_identity_now` or `append_lite3_identity_now` in
Rust, `append_identity` or `append_lite3_identity` in Python, `appendIdentity`
or `appendLite3Identity` in Node, and the `api.AppendIdentity` or
`api.AppendLite3Identity` helpers in Go. Existing full-message methods keep their
current behavior. Choose the numeric receipt when the producer only needs to
confirm its append; the consumer can still read the complete message.

The C ABI adds `plsm_pool_append_json_identity` and
`plsm_pool_append_lite3_identity` with caller-owned numeric output. These new
symbols require a matching library build that includes this addition. Rebuild
Go and Node with the updated SDK and distribute the matching native library.
Python keeps existing methods available with older libraries and raises
`NotImplementedError` before commit if an identity method's symbol is missing.
Go backends that implement only the existing Pool interface still compile;
identity helpers return an unsupported-operation error before they append.

## Follow replaces tail in the forthcoming 1.x release

The maintainer explicitly authorizes this breaking transition in 1.x. Replace
public tail methods and option types with follow. This change also changes the
default: a reader now skips stored messages and waits for new ones.

| Binding | Ordinary reader | View reader | Explicit history selection |
| --- | --- | --- | --- |
| Rust | `pool.follow(FollowOptions)` | `pool.follow_views(FollowOptions)` | `FollowStart::all()`, `Last(n)`, `Since(unix_ns)`, `Sequence(seq)` |
| Python | `pool.follow(...)` | `pool.follow_views(...)` | `FollowStart.all()`, `.last(n)`, `.since_ns(unix_ns)`, `.sequence(seq)` |
| JavaScript | `pool.follow(options)` | `pool.followViews(options)` | `FollowStart.all()`, `.last(n)`, `.since(unixNs)`, `.sequence(seq)` |
| Go | `pool.Follow(ctx, options)` | `pool.FollowViews(ctx, options)` | `local.All()`, `Last(n)`, `Since(unixNs)`, `Sequence(seq)` |

Pass the selection as Rust/Go `Start` (`start` in Rust), or Python/JavaScript
`start`. Go's zero-value options mean new messages. Python and JavaScript lazy
iterators open at the first iteration; creating an iterator alone does not start
listening. Rust local readers choose their position at construction. Go opens
its native reader before returning channels; HTTP opens before returning headers.

For example, explicitly read all retained Python messages and exit:

```python
from plasmite import FollowStart

for message in pool.follow(start=FollowStart.all(), finite=True):
    process(message)
```

In JavaScript, read the last ten retained messages and then keep listening:

```javascript
const { FollowStart } = require("plasmite");

for await (const message of pool.follow({ start: FollowStart.last(10) })) {
  process(message);
}
```

Use `finite=True` in Python, `finite: true` in JavaScript, `Finite: true` in Go,
or `live: false` in Rust for a finite history read. Finite mode alone selects all
retained history, like CLI `--no-follow`; an explicit last-zero finite selection
returns no messages. Finite reads stop at their opening upper sequence boundary,
even while writers append. Last N selects retained records before filters.
History remains subject to retention loss and the configured gap policy.

To resume after successfully processing message 100, select sequence 101. Save
application progress rather than the newest number in the pool. Timestamp
selectors accept absolute Unix nanoseconds; CLI relative times remain CLI input.

The corresponding low-level openings use follow names too: Python
`open_follow`/`open_lite3_follow`, JavaScript `openFollow`/`openLite3Follow`, and
Go `OpenFollow`/`OpenLite3Follow`/`OpenViewFollow`. Binary and bounded view
readers use the same selection and finite-history rules.

C callers zero-initialize `plsm_follow_options_t`, set `struct_size`, and call
`plsm_pool_follow`, `plsm_pool_follow_lite3` or `plsm_pool_follow_views`.
`start_kind` is 0 for new, 1 for last N, 2 for Unix nanoseconds or 3 for inclusive
sequence; `start_value` carries the selected value. Sequence zero requests all
retained history. `finite=1` selects finite history. Legacy C stream-open symbols
retain their published history-first behavior for existing binary callers.

HTTP clients use `/follow` and `/follow_lite3` with `start`, `start_value` and
`finite`. Old routes return not found. New clients require updated servers;
there is no fallback that silently replays stored messages. Existing UI/MCP
adapters may still send an explicit `since_seq`, but cannot mix it with `start`.

UDP uses the same selection vocabulary and finite defaults in the bindings;
its arrival-order, duplicate and delivery limits retain their existing meaning.
Go/Python/JavaScript UDP options now use `Finite`/`finite` in place of `Live`/`live`.
Rust retains `live: true` as its default for both local and UDP options.

Timeout, cancellation, immutable ownership and loss-reporting policies retain
their documented meanings. The [shared API contract](../../spec/api/v0/SPEC.md#follow-transition-authorized-1x-compatibility-exception)
owns the exact selection rules.
