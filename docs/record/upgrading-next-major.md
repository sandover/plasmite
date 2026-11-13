# Upgrade to the next major release

These changes exist in the source checkout and await a major release. Package
versions still identify the last release; this guide does not announce a release.
See the [changelog](../../CHANGELOG.md#unreleased) for the UDP delivery changes.

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
use plasmite::api::{Error, GapPolicy, Pool, PoolApiExt, TailOptions};
use std::time::Duration;

fn playback(pool: &Pool) -> Result<(), Error> {
    let mut tail = pool.tail(TailOptions {
        since_seq: Some(1), max_messages: Some(2),
        timeout: Some(Duration::from_secs(1)), gap_policy: GapPolicy::Error,
        ..TailOptions::default()
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
for message in pool.tail(since_seq=1, max_messages=2, timeout_ms=1000, error_on_gap=True):
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
for await (const message of pool.tail({
  sinceSeq: 1n, maxMessages: 2, timeoutMs: 1000, errorOnGap: true,
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
    messages, errs := pool.Tail(ctx, api.TailOptions{
        SinceSeq: &start, MaxMessages: &count, Timeout: time.Second, ErrorOnGap: true,
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
