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
| `feed --in jsonl`, `feed --in jq` | Omit the flag. Feed detects JSON objects from the input. |
| `feed --in lite3` | `feed --input-format lite3-raw` for a single attachment |
| `fetch POOL SEQ --format lite3` on 1.0 or 1.1 | Add `-raw` to keep writing a single raw attachment. In 2.0, `--format lite3` writes a stream. |
| `feed --data OBJECT` | `feed OBJECT`; ordinary objects are application data |
| `--errors skip` | Removed; bad input stops the command and earlier messages stay stored |
| `--timeout` (including on `duplex`) | Stop the process from outside: `timeout 30 plasmite follow chat` on Linux, `gtimeout` from GNU coreutils on macOS, or your process manager |
| `--where EXPR` | `jq 'select(EXPR)'` |
| `-f/--file FILE` | `< FILE` |
| `--create-size SIZE` | `--create` at the default size, or `pool create --size SIZE` first |
| `--retry`, `--retry-delay` | The caller retries |
| `--replay SPEED` | Your own timer; see [Control playback timing](#control-playback-timing) |
| `tap POOL -- CMD` | Run `CMD 2>&1 \| plasmite feed POOL --input-format lines` to combine stdout and stderr in one stream. Add a source field yourself if your reader needs to distinguish them. See [Capture command output](../cookbook.md#capture-command-output). |
| `--quiet-drops`, `--no-notify` | Remove the flags. Reads report detected retention gaps as `RetentionGap`. |
| `serve --shared-address URL` | `serve URL` |
| `serve --token`, `serve init`, `serve check` and other token flags | Named access keys; see [Share pools across machines](serving.md) |
| `plasmite mcp SERVER_URL` | Connect the MCP client to the server's HTTPS `/mcp` address; see [Connect an AI client](serving.md#connect-an-ai-client) |

`fetch POOL SEQ` and `follow --one` work as before. In Bash, use `set -o pipefail` when a captured command's failure should fail the pipeline.

Framed Lite3 streams (`--format lite3` and `feed --input-format lite3`) start with `PLS2`. To import a saved `PLS1` stream, use a 1.2 or 1.3 executable to `feed` it into a scratch pool with `--input-format lite3 --format json`. Feed those JSON receipts into your 2.0 destination. The destination assigns new sequences and times.

## Library calls

Every binding shares one model: `feed` writes, `follow` waits for new messages, and `fetch` reads what the pool holds and exits. Both reads start at `since` (a sequence, `oldest`, a time or a duration) or `last`, and filter by `tags`. Across bindings:

- `time` is integer Unix nanoseconds. 1.x gave a `datetime`, `Date`, `time.Time` or RFC 3339 string, plus separate nanosecond fields.
- A read that falls behind retention raises `RetentionGap` with `missing` and `next_seq`. 1.x skipped lost messages by default.
- Read options select where to start. For the old message cap, count delivered messages and close the reader when you reach the cap. For a timeout, close or cancel the reader at your deadline, or use the binding's bounded wait for each call. `last` selects a starting position.
- Reading one message by sequence means opening `fetch` at that sequence and taking the first message.
- Open-or-create helpers become `open_pool(ref, create=True)` or its equivalent. Use `create_pool` separately if a new pool needs a particular size.

**Python**

| 1.x | 2.0 |
| --- | --- |
| `append`, `append_json`, `append_lite3` | `feed`, with `format="json"` or `format="lite3"` for bytes |
| `get(seq)`, `get_json`, `get_lite3` | Take the first item in a `with pool.fetch(since=seq) as reader:` block. Use `message.raw` for the stored Lite3 bytes. |
| `tail(...)`, `open_stream`, `open_lite3_stream` | `follow(...)` or `fetch(...)` |
| `replay(speed=...)` | `fetch()` and your own timer |
| `since_seq=N`, `max_messages`, `timeout_ms`, `error_on_gap` | Use `since=N`, count results yourself and use `reader.next(wait=seconds)` for a bounded wait. Retention gaps always raise an error. |
| `Client.pool(ref, size)` | `Client.open_pool(ref, create=True)` |
| `message.time` (`datetime`), `time_rfc3339` | `message.time` (nanoseconds) |
| `UdpPool.send_json`, `send_lite3` | `UdpPool.feed` with the input format when passing bytes |
| `UdpPool.follow(UdpFollowOptions(...))` | `UdpPool.follow(since=..., last=...)` or `fetch(...)` |

**JavaScript**

| 1.x | 2.0 |
| --- | --- |
| `append`, `appendJson`, `appendLite3` | `feed`, with `format: "json"` or `format: "lite3"` for bytes |
| `get(seq)`, `getJson` | The first message of `await pool.fetch({ since: seq })` |
| `tail(options)`, `replay(options)`, `openStream` | `follow(options)` or `fetch(options)` |
| `sinceSeq`, `maxMessages`, `timeoutMs`, `errorOnGap` | Use `since`, count results yourself and pass a `signal` or call `reader.next(waitSeconds)`. Retention gaps always raise an error. |
| `client.pool(ref, size)` | `client.openPool(ref, { create: true })` |
| `message.time` (`Date`), `timeRfc3339`, `Lite3Frame.timestampNs` | `time` (`bigint` nanoseconds) |
| `RemotePool.append`, `get`, `tail` | `RemotePool.feed`, `fetch`, `follow` |
| `UdpPool.sendJson`, `follow({ start: UdpStart... })` | `UdpPool.feed`, `follow({ since, last })` or `fetch(...)` |

**Go**

| Earlier call or field | 2.0 |
| --- | --- |
| `Append`, `AppendOption` | `Feed`, `FeedOption` |
| `AppendJSON`, `FeedJSON`, `FeedJSONView` | `Feed(plasmite.JSON(payload), tags)` |
| `AppendLite3`, `FeedLite3` | `Feed(plasmite.Lite3(payload), nil)` |
| `FeedReceipt`, `FeedJSONReceipt`, `FeedLite3Receipt` | Call `Seq()` and `Time()` on the message from `Feed` |
| `FeedView` | `Feed(value, tags)` for new data or `Feed(message, nil)` for a forward |
| `Get(seq)`, `GetJSON`, `GetLite3` | Take the first message of `Fetch(api.Since(seq))`. Use `json.Marshal(message)` for the JSON envelope or `message.Raw()` for Lite3. |
| `Tail`, `TailLite3`, `Replay`, `OpenStream`, `OpenLite3Stream`, `FollowLite3`, `FollowView`, `FetchLite3`, `FetchView` | Open `Follow` or `Fetch` and read with `Next(ctx)` |
| `TailOptions{SinceSeq, MaxMessages, Timeout}` | Use `api.Since`, count results yourself and pass a context deadline to `Next`. |
| `Client.Pool(ref, size)` | `Client.OpenPool(ref, api.Create(true))` |
| `message.Seq`, `message.Time`, `message.Data`, `message.Meta.Tags` | `message.Seq()`, `message.Time()` (`uint64` nanoseconds), `message.Data()`, `message.Tags()` |
| `TimeRFC3339`, `message.Raw` (JSON envelope), `view.Lite3()` | Use `json.Marshal(message)` for the RFC 3339 envelope or `message.Raw()` for Lite3 |
| `Prepare` and `selector.Read` | Decode selected fields with `message.Decode(&target)` where `target` is a struct |
| `udp.Pool.SendJSON`, `SendLite3`, `FeedLite3` | `Feed(plasmite.JSON(payload), tags)` or `Feed(plasmite.Lite3(payload), nil)` |
| `udp.Follow(NewFollowOptions())`, `ViewFrame` | Open `Follow(opts...)` or `Fetch(opts...)`. Each `*udp.Frame` carries the record in `frame.Message`. |

**Rust**

| 1.x | 2.0 |
| --- | --- |
| `append_json`, `append_json_now`, `append_lite3` | `feed`, with `FeedInput::Json` or `FeedInput::Lite3` for bytes |
| `get_message(seq)` | The first message of `fetch(Read::since(Since::Seq(seq)))` |
| `tail(TailOptions)`, `replay(ReplayOptions)` | `follow(Read)` or `fetch(Read)` |
| `TailOptions::gap_policy` | Removed; reads always report `RetentionGap` |
| `create_pool` returns `PoolInfo` | `create_pool` returns a pool handle |
| `Message.time` (RFC 3339 `String`) | `Message.time` (`u64` nanoseconds) |

**Earlier 2.0 development builds**

| Former operation | Use in 2.0 |
| --- | --- |
| Python/Rust `feed_json`, JavaScript `feedJson` | `feed` with explicit JSON input |
| Python/Rust `feed_lite3`, JavaScript `feedLite3` | `feed` with explicit Lite3 input |
| `feed_view` / `feedView`, including JSON-to-view forms | `feed` with a message or the matching explicit input |
| Numeric receipt forms of any feed | Read sequence and time from the returned message |
| `follow_view` / `followView` and `fetch_view` / `fetchView` | `follow` and `fetch` return immutable messages |
| `follow_lite3` / `followLite3` and `fetch_lite3` / `fetchLite3` | `follow` and `fetch`, then read the stored bytes from the message |
| Python `next_batch` | Iterate the reader or call `reader.next(wait=...)`. Collect messages in your own batch when the next operation takes several at once. |
| Python or JavaScript view data and conversion helpers | Read `message.data` for the full object and `message.field(name)` for one field. Use `message.raw` for stored bytes. |
| Rust `MessageView` reads | `Message::field` reads a field, `Message::decode` reads into a Rust type, and `Message::data` reads the full JSON value. |

Python, Go and the Node add-on check the native library's major version when they load it. The library names are `libplasmite.so.2` on Linux, `libplasmite.2.dylib` on macOS and `plasmite2.dll` on Windows.

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

Plasmite 2.0 accepts MCP `2025-11-25` and `2026-07-28` over stdio and HTTPS. For HTTPS, use a client that supports Client ID Metadata Documents and follow the [agent setup guide](../agents.md#connect-an-agent).

If you previously signed in through Dynamic Client Registration, sign in again with your existing Plasmite access key. Your old OAuth token keeps its original expiration. Renewal requires a new sign-in with a client that supports client metadata documents.

| 1.x tool or field | 2.0 |
| --- | --- |
| `plasmite_fetch {pool, seq: N}` | `plasmite_fetch {pool, since: N, count: 1}` |
| `plasmite_read` | `plasmite_fetch` |
| `plasmite_wait` | `plasmite_follow` |
| `after_seq: N` | `since: N + 1` |
| `next_after_seq` | `next_seq`; pass it unchanged as the next `since` |
| `fell_behind` | A `RetentionGap` error carrying `missing` and `next_seq` |

`plasmite_fetch` kept its name and changed its arguments, so a 1.x prompt or script that sends `{seq: N}` fails with an unknown-argument error. With no start or tags, 1.x `plasmite_read` returned the latest 20 messages. Pass `last: 20` to read that window in 2.0. With tags, `last` still counts all records before filtering. Increase the window if you need to search further back. Each fetch result carries `ended`: `complete` when it reaches the opening end, or `limit` when it reaches a batch limit. A reader that accepts incomplete history can resume at the `next_seq` in a `RetentionGap` error.

## Update server startup commands

Remove `--max-tail-concurrency` and `--max-tail-timeout-ms` from commands that start a foreground server. The 2.0 server sets those limits itself. Keep `--max-body-bytes` if you need a custom request size.

For an installed server, run the new executable's `plasmite --dir DIR serve install` for its existing pool directory. The update reads the saved settings and rewrites the startup command without the retired flags.

## Upgrade HTTP clients and servers together

A 2.0 server answers old versioned pool routes with an error that names the required protocol version. New client requests go directly to `/pools` and the server includes `Plasmite-Version: 2` in its responses.

| 1.x route | 2.0 route |
| --- | --- |
| `GET /v0/pools`, `POST /v0/pools` | `GET /pools`, `POST /pools` |
| `GET /v0/pools/{p}/info` | `GET /pools/{p}` |
| `DELETE /v0/pools/{p}` | `DELETE /pools/{p}` |
| `POST /v0/pools/open` | `POST /pools/{p}/open` |
| `POST /v0/pools/{p}/append` | `POST /pools/{p}/feed` |
| `POST /v0/pools/{p}/append_lite3` | `POST /pools/{p}/feed` with `Content-Type: application/x-plasmite-lite3` |
| `GET /v0/pools/{p}/messages/{seq}` | First record of `GET /pools/{p}/fetch?since={seq}` |
| `GET /v0/pools/{p}/messages/{seq}/lite3` | The same, with `Accept: application/x-plasmite-lite3-stream` |
| `GET /v0/pools/{p}/tail` | `GET /pools/{p}/follow`, or `/fetch` for a finite read |
| `GET /v0/pools/{p}/tail_lite3` | The same, with `Accept: application/x-plasmite-lite3-stream` |
| `since_seq`, `max`, `timeout_ms`, `gap_policy` query parameters | `since`, `last`, repeated `tag`; the client closes the response to stop |

Sign in again in the browser after upgrading. The browser uses `/browser/session` and gets a cookie that reaches the new pool routes.

An open read that reaches a server ending carries one control record when the connection can deliver it. `complete` ends a fetch normally. `error` carries the failure, such as `RetentionGap` with `missing` and `next_seq`. `cutoff` means the server reached its time limit. Send a new fetch with `since` set to its `next_seq`, the same `until`, and `incarnation` set to the `Plasmite-Incarnation` header from the first response. A response that ends on its own without a control record failed. See the [HTTP contract](../../spec/v2/SPEC.md#http).

## Rebuild C callers

Rebuild C programs against the 2.0 header.

| 1.x C call | 2.0 C call |
| --- | --- |
| `plsm_pool_append_json`, `plsm_pool_append_lite3` | `plsm_pool_feed` with `PLSM_FEED_JSON` or `PLSM_FEED_LITE3` and `plsm_feed_options_t` |
| `plsm_pool_get_json`, `plsm_pool_get_lite3` | Open `plsm_pool_fetch` at the sequence and take the first message. Use `plsm_message_raw` for stored bytes. |
| `plsm_stream_open(_ex)`, `plsm_lite3_stream_open(_ex)` | `plsm_pool_follow` or `plsm_pool_fetch`, with `plsm_read_options_t` |
| `plsm_stream_next`, `plsm_lite3_stream_next` | `plsm_stream_next_message`, with a timeout in milliseconds |
| Earlier 2.0 `plsm_stream_next_wait`, `plsm_lite3_stream_next_wait`, `plsm_view_stream_next` | Use `plsm_stream_next_message` for one message. Read its bytes with `plsm_message_raw` or use `plsm_message_to_json` for the envelope. |
| Earlier 2.0 `plsm_pool_feed_json`, `plsm_pool_feed_lite3`, view and receipt variants | Use `plsm_pool_feed` with the input kind. Read the sequence and time from the returned message, then release it with `plsm_message_release`. |
| `plsm_stream_options_t`, `plsm_gap_policy_t` | Removed; reads always report `PLSM_ERROR_RETENTION_GAP` |
| `plsm_udp_pool_send_json`, `plsm_udp_pool_send_lite3` | `plsm_udp_pool_feed` with `PLSM_FEED_JSON` or `PLSM_FEED_LITE3` |
| `plsm_udp_pool_follow`, `plsm_udp_pool_read` | `plsm_udp_pool_follow` or `plsm_udp_pool_fetch`. Read messages and control events with `plsm_udp_follow_next_view`. |

## Upgrade Go imports

Add `/v2` after `bindings/go` in each Plasmite import. For example, change the local package to `github.com/sandover/plasmite/bindings/go/v2/local`. Install the 2.x native SDK and run `go get github.com/sandover/plasmite/bindings/go/v2/local@v2.0.0` from your application. The new path lets projects pin the breaking API by version.

## Duplex sends and receives ordinary messages

`duplex` uses the same input parser as `feed`, including at a terminal. Enter JSON objects, complete messages, marked YAML or framed Lite3. Remove `--me` and `--echo-self`. The receive stream includes the process's own messages, so use `meta.sender` for access-key names and put any other application identity in `data` or tags.

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

- **One owned message per result.** `feed`, `follow`, and `fetch` return messages that keep their stored bytes. Access sequence, time, tags, and sender without decoding data. Forward a message by passing it to `feed`.
- **Read-only keys and sender names.** Use `access invite --read-only` for a reader. On a keyed write, the server stores the key name with the message.
- **Pool descriptions.** Use `pool create --about` and `--example` to explain what a pool carries.
- **Compact terminal messages.** `follow`, `fetch`, and `duplex` use one line per message in a terminal. Choose `--format pretty` for complete indented JSON.
