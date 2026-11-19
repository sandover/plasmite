# Upgrade to Plasmite 2.0

Plasmite 2.0 opens existing pool files in place. Upgrade HTTP clients and servers together, rebuild against the C library if you link it, and update the calls and commands below. This guide covers every 1.x release, 1.0 through 1.3.

## Commands

| 1.x | 2.0 |
| --- | --- |
| `follow --no-follow` | `fetch` |
| `--tail N` | `--last N` (`-n` stays). It counts records before the tag filter. |
| `--json`, `--jsonl`, `--format jsonl` | `--format json`, or omit it: pipes and files get JSON by default |
| `--yaml` | `--format yaml` |
| `--data-only` | `jq -c '.data'` |
| `feed --in FORMAT` | `feed --input-format FORMAT`, needed only when automatic detection can't tell |
| `feed --data OBJECT` | `feed OBJECT`; ordinary objects are application data |
| `--errors skip` | Removed; bad input stops the command and earlier messages stay stored |
| `--timeout` (including on `duplex`) | Stop the process from outside: `timeout 30 plasmite follow chat` on Linux, `gtimeout` from GNU coreutils on macOS, or your process manager |
| `--where EXPR` | `jq 'select(EXPR)'` |
| `-f/--file FILE` | `< FILE` |
| `--create-size SIZE` | `--create` at the default size, or `pool create --size SIZE` first |
| `--retry`, `--retry-delay` | The caller retries |
| `--replay SPEED` | Your own timer; see [Control playback timing](#control-playback-timing) |
| `tap -- CMD` | A pipe into `feed --input-format lines`; see [Capture command output](../cookbook.md#capture-command-output) |
| `--quiet-drops`, `--no-notify` | Removed; reads report every loss as `RetentionGap` |
| `serve --shared-address URL` | `serve URL` |
| `serve --token`, `serve init`, `serve check` and other token flags | Named access keys; see [Share pools across machines](serving.md) |
| `plasmite mcp SERVER_URL` | Connect the MCP client to the server's HTTPS `/mcp` address; see [Connect an AI client](serving.md#connect-an-ai-client) |

`fetch POOL SEQ` and `follow --one` work as before. In Bash, use `set -o pipefail` when a captured command's failure should fail the pipeline.

Framed Lite3 streams (`--format lite3` and `feed --input-format lite3`) start with `PLS2`, and 2.0 rejects the 1.x `PLS1` prefix. To carry a saved `PLS1` stream into 2.0, read it with 1.x as JSON and feed that JSON into 2.0.

## Library calls

Every binding shares one model: `feed` writes, `follow` waits for new messages, and `fetch` reads what the pool holds and exits. Both reads start at `since` (a sequence, `oldest`, a time or a duration) or `last`, and filter by `tags`. Across bindings:

- `time` is integer Unix nanoseconds. 1.x gave a `datetime`, `Date`, `time.Time` or RFC 3339 string, plus separate nanosecond fields.
- A read that falls behind retention raises `RetentionGap` with `missing` and `next_seq`. 1.x skipped lost messages by default.
- Timeouts and message limits are gone from read options. Leave the loop, cancel, or pass a wait to `next`.
- Reading one message by sequence means opening `fetch` at that sequence and taking the first message.
- Open-or-create helpers become `open_pool(ref, create=True)` or its equivalent.

**Python**

| 1.x | 2.0 |
| --- | --- |
| `append`, `append_json`, `append_lite3` | `feed`, `feed_json`, `feed_lite3` |
| `get(seq)`, `get_json`, `get_lite3` | `next(iter(pool.fetch(since=seq)))`, or `fetch_lite3` |
| `tail(...)`, `open_stream`, `open_lite3_stream` | `follow(...)` or `fetch(...)`, and their `_lite3` forms |
| `replay(speed=...)` | `fetch()` and your own timer |
| `since_seq=N`, `max_messages`, `timeout_ms`, `error_on_gap` | `since=N`, `last=N`; `reader.next(wait=seconds)` |
| `Client.pool(ref, size)` | `Client.open_pool(ref, create=True)` |
| `message.time` (`datetime`), `time_rfc3339` | `message.time` (nanoseconds) |
| `UdpPool.send_json`, `send_lite3` | `UdpPool.feed`, `feed_lite3` |
| `UdpPool.follow(UdpFollowOptions(...))` | `UdpPool.follow(since=..., last=...)` or `fetch(...)` |

**JavaScript**

| 1.x | 2.0 |
| --- | --- |
| `append`, `appendJson` | `feed`, `feedJson`, which return promises |
| `get(seq)`, `getJson` | The first message of `await pool.fetch({ since: seq })` |
| `tail(options)`, `replay(options)`, `openStream` | `follow(options)` or `fetch(options)` |
| `sinceSeq`, `maxMessages`, `timeoutMs`, `errorOnGap` | `since`, `last`, `signal`; `reader.next(wait)` |
| `client.pool(ref, size)` | `client.openPool(ref, { create: true })` |
| `message.time` (`Date`), `timeRfc3339`, `Lite3Frame.timestampNs` | `time` (`bigint` nanoseconds) |
| `RemotePool.append`, `get`, `tail` | `RemotePool.feed`, `fetch`, `follow` |
| `UdpPool.sendJson`, `follow({ start: UdpStart... })` | `UdpPool.feed`, `follow({ since, last })` or `fetch(...)` |

**Go**

| 1.x | 2.0 |
| --- | --- |
| `Append`, `AppendJSON`, `AppendLite3`, `AppendOption` | `Feed`, `FeedJSON`, `FeedLite3`, `FeedOption` |
| `Get(seq)`, `GetJSON`, `GetLite3` | The first message of `Fetch(api.Since(seq))` |
| `Tail`, `TailLite3` (channels), `Replay`, `OpenStream`, `OpenLite3Stream` | `Follow`, `Fetch` and their `Lite3` forms, read with `Next(ctx)` |
| `TailOptions{SinceSeq, MaxMessages, Timeout}` | `api.Since`, `api.Last`, `api.Tags`; a context deadline |
| `Client.Pool(ref, size)` | `Client.OpenPool(ref, api.Create(true))` |
| `Message.Time` (`time.Time`), `TimeRFC3339` | `Message.Time` (`uint64` nanoseconds) |
| `udp.Pool.SendJSON`, `SendLite3`, `Follow(NewFollowOptions())` | `Feed`, `FeedLite3`, `Follow(opts...)` or `Fetch(opts...)` |

**Rust**

| 1.x | 2.0 |
| --- | --- |
| `append_json`, `append_json_now`, `append_lite3` | `feed`, `feed_json`, `feed_lite3` with `Feed` options |
| `get_message(seq)` | The first message of `fetch(Read::since(Since::Seq(seq)))` |
| `tail(TailOptions)`, `replay(ReplayOptions)` | `follow(Read)` or `fetch(Read)` |
| `TailOptions::gap_policy` | Removed; reads always report `RetentionGap` |
| `create_pool` returns `PoolInfo` | `create_pool` returns a pool handle |
| `Message.time` (RFC 3339 `String`) | `Message.time` (`u64` nanoseconds) |

Python, Go and the Node add-on check the native library's major version when they load it. The library names are `libplasmite.so.2` on Linux, `libplasmite.2.dylib` on macOS and `plasmite2.dll` on Windows.

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

If 101 has already expired, `follow` raises `RetentionGapError` before delivering anything. The pool knows which sequence numbers it lost, but not whether any of them carried canvas tags, so the app can no longer verify that its canvas is complete. It needs a known state, such as a fresh snapshot from wherever the canvas is saved, before it applies more updates.

Keep a checkpoint with the pool it came from. Discard it if you replace the pool, because the new pool assigns its own sequences starting at 1.

## Control playback timing

Read history with `fetch` and use your language's timer to pace each message. This Python loop processes retained messages ten times faster than their original timing:

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

A clock step can make the next message's timestamp smaller. This loop continues at once when that happens and keeps the pool's sequence order.

## Migrate MCP clients

| 1.x tool or field | 2.0 |
| --- | --- |
| `plasmite_fetch {pool, seq: N}` | `plasmite_fetch {pool, since: N}`, then take the first message |
| `plasmite_read` | `plasmite_fetch` |
| `plasmite_wait` | `plasmite_follow` |
| `after_seq: N` | `since: N + 1` |
| `next_after_seq` | `next_seq`; pass it unchanged as the next `since` |
| `fell_behind` | A `RetentionGap` error carrying `missing` and `next_seq` |

`plasmite_fetch` kept its name and changed its arguments, so a 1.x prompt or script that sends `{seq: N}` fails with an unknown-argument error. With no `since`, 1.x `plasmite_read` returned the latest 20 messages; 2.0 `plasmite_fetch` starts at the oldest, so pass `last: 20` for the old behavior. Each fetch result carries `ended`: `complete` when it reached the end of what the pool held at opening, `limit` when it hit `count`. A reader that accepts incomplete history can resume at the `next_seq` in a `RetentionGap` error.

## Upgrade HTTP clients and servers together

A 2.0 server answers 1.x routes with an error that names the required protocol version, and a 2.0 client reports a 1.x server the same way.

| 1.x route | 2.0 route |
| --- | --- |
| `GET /v0/pools`, `POST /v0/pools` | `GET /v2/pools`, `POST /v2/pools` |
| `GET /v0/pools/{p}/info` | `GET /v2/pools/{p}` |
| `DELETE /v0/pools/{p}` | `DELETE /v2/pools/{p}` |
| `POST /v0/pools/open` | `POST /v2/pools/{p}/open` |
| `POST /v0/pools/{p}/append` | `POST /v2/pools/{p}/feed` |
| `POST /v0/pools/{p}/append_lite3` | `POST /v2/pools/{p}/feed` with `Content-Type: application/x-plasmite-lite3` |
| `GET /v0/pools/{p}/messages/{seq}` | First record of `GET /v2/pools/{p}/fetch?since={seq}` |
| `GET /v0/pools/{p}/messages/{seq}/lite3` | The same, with `Accept: application/x-plasmite-lite3-stream` |
| `GET /v0/pools/{p}/tail` | `GET /v2/pools/{p}/follow`, or `/fetch` for a finite read |
| `GET /v0/pools/{p}/tail_lite3` | The same, with `Accept: application/x-plasmite-lite3-stream` |
| `since_seq`, `max`, `timeout_ms`, `gap_policy` query parameters | `since`, `last`, repeated `tag`; the client closes the response to stop |

Every read ends with one control record. `complete` ends a fetch normally. `error` carries the failure, such as `RetentionGap` with `missing` and `next_seq`. `cutoff` means the server's time limit ended a fetch early: send a new fetch with `since` set to its `next_seq`, the same `until`, and `incarnation` set to the `Plasmite-Incarnation` header from the first response, so the server can confirm the pool wasn't replaced. A response that ends without a control record failed. See the [HTTP contract](../../spec/v2/SPEC.md#http).

## Rebuild C callers

Rebuild C programs against the 2.0 header.

| 1.x C call | 2.0 C call |
| --- | --- |
| `plsm_pool_append_json`, `plsm_pool_append_lite3` | `plsm_pool_feed_json`, `plsm_pool_feed_lite3`, with `plsm_feed_options_t` |
| `plsm_pool_get_json`, `plsm_pool_get_lite3` | Open `plsm_pool_fetch` or `plsm_pool_fetch_lite3` at the sequence and take the first record |
| `plsm_stream_open(_ex)`, `plsm_lite3_stream_open(_ex)` | `plsm_pool_follow`, `plsm_pool_fetch` or their `_lite3` forms, with `plsm_read_options_t` |
| `plsm_stream_next`, `plsm_lite3_stream_next` | `plsm_stream_next_wait`, `plsm_lite3_stream_next_wait`, with a timeout in milliseconds |
| `plsm_stream_options_t`, `plsm_gap_policy_t` | Removed; reads always report `PLSM_ERROR_RETENTION_GAP` |
| `plsm_udp_pool_send_json`, `plsm_udp_pool_send_lite3` | `plsm_udp_pool_feed_json`, `plsm_udp_pool_feed_lite3` |
| `plsm_udp_pool_follow` | `plsm_udp_pool_read` |

## Upgrade Go imports

Add `/v2` after `bindings/go` in each Plasmite import. For example, change the local package to `github.com/sandover/plasmite/bindings/go/v2/local`. Install the 2.x native SDK and run `go get github.com/sandover/plasmite/bindings/go/v2/local@v2.0.0` from your application. The new path lets projects pin the breaking API by version.

## Duplex sends and receives ordinary messages

`duplex` uses the same input parser as `feed`, including at a terminal. Enter JSON objects, complete messages, marked YAML or framed Lite3. Duplex delivers every matching message, including its own. Put any application-specific identity in `data` or tags.

`plasmite duplex chat` opens its reader before it sends its first message. Input EOF closes sending, so a program can send a request, close stdin, and still read a reply. Stop the process or close stdout when it has what it needs. Use `feed` for a finite copy that exits at input EOF.

## JSON reads preserve stored floating-point values

Rust parses JSON floats with correct rounding, and C JSON reads format stored values directly. 1.x could round a value a second time during a read, turning stored `2.027716612057172e155` into `2.0277166120571721e155`. If a test fixture captured that extra rounding, regenerate it from the stored value.

## Default pools stay in the user's home directory

The default stays `~/.plasmite/pools` (`%USERPROFILE%\.plasmite\pools` on Windows), and every client now finds it the same way. Unix clients use an absolute, nonempty `HOME`, then the current account's home. Windows clients use an absolute `USERPROFILE`, then the platform's profile directory. Empty or relative values never select a relative directory. If no home resolves, pass `--dir PATH` or an explicit client directory. Installed services keep their configured directories.

Pools already in the home directory need no migration. Some 1.x clients chose other places when home resolution failed: Rust clients with no `HOME` created `.plasmite/pools` under the working directory, Go clients could pick a temporary `plasmite/pools` directory, and Windows Rust clients followed `HOME` instead of `USERPROFILE`. Inspect those places before you upgrade. Select an old location explicitly, or stop its writers and move the pool files to your chosen directory; Plasmite never moves files or searches other locations.

Rust's `LocalClient::pool_dir()` returns `Result<&Path, Error>`, and Go's `local.DefaultPoolDir()` returns `(string, error)`; handle the missing-home error. Node's `DEFAULT_POOL_DIR` and Python's default are `undefined` or `None` when home resolution fails, and explicit clients still work.

## Node error fields use bigint

`PlasmiteNativeError` and `RemoteError` expose optional `seq` and `offset` fields as `bigint`, so IDs above JavaScript's safe-integer limit stay exact. Compare with bigint literals and convert to strings before you serialize them:

```javascript
if (error.seq !== undefined) {
  console.error(`Message ${error.seq.toString()} failed`);
}
```

## Name local paths explicitly

C, Node and Python tell names from paths by native path separators. Use `./work.plasmite` (or `.\work.plasmite` on Windows) for a file in the working directory; `work.plasmite` names a pool in the client's pool directory. Absolute Windows drive paths, UNC paths and backslash-relative paths reach the local file API; drive-relative forms such as `C:work.plasmite` don't. Go's `PoolRefPath("work.plasmite")` adds the `./` prefix for you.

## New in 2.0

- **Owned views** read selected fields from one immutable Lite3 snapshot without decoding the whole message, and forward messages without re-encoding. See the [Rust](../../README-crate.md), [Python](../../bindings/python/README.md), [JavaScript](../../bindings/node/README.md) and [Go](../../bindings/go/README.md) guides.
- **Numeric feed receipts** return only the committed sequence and time, for producers that don't need the stored message back. The same guides list each binding's receipt calls.
