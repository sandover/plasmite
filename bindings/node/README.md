# plasmite

Persistent, bounded JSON message streams for Node.js. Local use needs no broker or daemon.

Plasmite gives you disk-backed ring buffers ("pools") that multiple processes
can read and write concurrently. Use them for IPC, recent event history,
and process output. The ring overwrites old messages when full; it does not
acknowledge consumption or retain work until a consumer finishes.

## Install

```bash
npm install plasmite
```

Requires **Node.js 24 or newer**, including the npm CLI. Published packages
include native binaries on macOS x64/arm64, Linux x64, and Windows x64, so
those platforms need no Rust toolchain or compile step. See
[platform support](#platform-support) before installing on another target.

## Quick start

### Local pools (native, in-process)

```js
const { Client, Durability, ErrorKind, PlasmiteNativeError } = require("plasmite");

const client = new Client("./data");
let pool;
try {
  pool = client.pool("events", 64 * 1024 * 1024);

  const msg = pool.append({ kind: "signup", user: "alice" }, ["user-event"], Durability.Flush);
  console.log(msg.seq, msg.tags, msg.data);
  // => 1n [ 'user-event' ] { kind: 'signup', user: 'alice' }

  const fetched = pool.get(msg.seq);
  console.log(fetched.data.user);
  // => "alice"

  try {
    client.openPool("missing");
  } catch (err) {
    if (err instanceof PlasmiteNativeError && err.kind === ErrorKind.NotFound) {
      console.log("pool not found");
    } else {
      throw err;
    }
  }
} finally {
  if (pool) pool.close();
  client.close();
}
```

### Owned views (additive source API)

`await pool.getView(sequence)` returns an immutable Lite3 snapshot. Properties
read the stored data; arrays support normal indexing, iteration and shallow
methods. Children remain valid after pool close or ring overwrite.

```js
const { toNative, toJson, toLite3 } = require("plasmite");
const message = await pool.getView(17n);
const metrics = message.data.metrics;
console.log(metrics.power);
for (const sample of message.data.samples) console.log(sample.value);
const editable = toNative(metrics); // Independent mutable subtree.
editable.power = 42;
const json = toJson(message);       // Direct formatting, exact integer digits.
const encoded = toLite3(message);   // Detached mutable Buffer; identity stays separate.
```

Use `pool.getView<Reading>(17n)` in TypeScript to declare an expected data shape;
this assertion performs no runtime validation. `toNative(message)` creates the
canonical envelope with a string time; subtree conversion preserves its shape.
Safe stored integers read as numbers and larger i64 values as bigints. Use
`toJson` for exact JSON; ordinary JSON.stringify can reject bigint data.

Object views have no Object prototype methods; use Object.hasOwn. Spread and
slice make shallow containers whose children remain views. Mutation, freeze and
seal throw TypeError; structuredClone cannot clone these proxies. Native copies
preserve __proto__ as an own data property.

Concurrent getView calls serialize off-thread. Closing a pool lets already
queued reads finish. A legacy synchronous operation during an active async read
returns Busy rather than waiting on the JavaScript thread. Await the read before
mixing those operations. An older shared library lacking the additive view ABI
produces an actionable error; install matching native assets.

Views avoid full decoding for selected reads, but property access still crosses
a language boundary. Native conversion and small-record latency have different
costs from legacy methods. See the
[measured comparisons](../../docs/performance/language-boundary-results/node-views/README.md).
Run `node --expose-gc scripts/bench_views.js 100 5 result.json` from this directory
after staging matching optimized native assets to repeat the focused comparison.

### Owned local streams

```js
const controller = new AbortController();
for await (const message of pool.tailViews({
  tags: ["sensor"], sinceSeq: 1n, signal: controller.signal,
})) {
  console.log(message.data.metrics.power);
  break; // Closes this reader; retained messages remain readable.
}
```

`tailViews` pulls one snapshot at a time without hidden prefetch or batching.
It shares local tag filtering, gap policy and per-wait timeout behavior with
`tail`. Abort the signal
when abandoning a pending read; breaking the iterator also cancels its wait.
UDP `await udpPool.followViews(options)` uses the same follow lifecycle as
`follow`; frame events carry the view in `event.message`. Gap and progress
events keep their existing shape. Close the follow in `finally`.

### Native input and committed views (additive source API)

```js
const sent = await pool.appendView({
  metrics: { power: 42 }, bytes: Buffer.from([1, 2]), counter: 9223372036854775807n,
}, ["sensor"]);
const forwarded = await destination.appendView(sent); // Reuse its encoding and tags.
const retagged = await destination.appendView(sent, ["processed"]);
const copied = await destination.appendView(sent.data); // New data object, no tags.
```

`appendView` evaluates input once on the calling thread, queues storage work,
and returns an immutable view of the committed encoding. Mutating the input
later cannot change the receipt. Closing a pool during a getter or serializer,
or after queuing the append, keeps the in-flight target alive. Concurrent calls
serialize storage work off-thread.

Ordinary objects follow JSON's getter, Proxy, `toJSON`, own-key and array-hole
rules. Object properties with undefined/functions/symbols disappear; array
positions become null. Exact built-in Buffer/Uint8Array values encode native
bytes instead of Buffer's default JSON object. Custom serializers and subclass
serializers still run once. Ordinary byte buffers snapshot synchronously;
shared-backed buffers require a caller-created ordinary copy, such as
`Uint8Array.from(sharedBytes)`. Native input accepts signed-i64 BigInt and finite
numbers; safe integral numbers use i64 except negative zero. A small BigInt can
read back as number. Cycles, out-of-range BigInt, nonfinite numbers and isolated
UTF16 surrogates fail before publication. Getter/serializer exceptions retain
their original thrown value. Buffered primitive writes can report resource or
encoded-size failures when a batch flushes or finishes; no message publishes
on failure. Type, key and depth checks keep their traversal order.

Data must encode as an object. String input does not mean JSON source. A genuine
message view forwards its data and existing tags when tags are omitted; explicit
tags rebuild its payload. A payload subtree copies directly through the native
writer, including exact integers and bytes. TypeScript's optional
`appendView<Reading>(input)` type argument asserts the output shape without
runtime validation. Existing `append`, `appendJson` and parseMessage behavior
stay compatible.

### Owned local and UDP streams (additive source API)

```js
for await (const message of pool.tailViews({ tags: ["sensor"], maxMessages: 10 })) {
  console.log(message.data.metrics.power);
}
const follow = await udpPool.followViews();
try {
  for await (const event of follow) {
    if (event.kind === "frame") console.log(event.message.data.metrics.power);
  }
} finally { follow.close(); }
```

`tailViews<Reading>()` and `followViews<Reading>()` carry immutable snapshots;
retained children survive iterator cleanup, pool overwrite and handle close.
UDP frame events contain `message`, sequence and nanosecond time. They omit the
separate mutable payload Buffer. Gap and progress events keep their existing
meaning and fields. Each iterator reads on demand and creates no default
prefetch queue. A supplied type argument asserts the payload shape.

For finite `tailViews`, `timeoutMs` bounds each read, as for `tail()`. A finite
tail ends after a quiet wait. An unlimited tail keeps one native read pending
until data or cancellation, without resolving empty JavaScript Promises. Required tags filter
before the matching-message limit. `signal` cancels a pending local
read. Calling `return()` on an idle owned iterator interrupts its read, so
explicit cleanup completes without waiting for data. UDP `nextEvent` aborts
only that wait; the follower can read again. `cancel()` permanently cancels the
follower, and `close()` releases it after any active call finishes.

Async reads first check for available data on four private workers, leaving
Node's shared filesystem/DNS workers available. An empty read waits on a parked
thread for that handle. The next consumer demand checks the shared workers
again, so a backlog does not wake every stream's thread for every record.
UDP reads use the parked socket waiter directly; an extra readiness probe adds
socket-mode changes and another handoff before network arrival. Each handle
reuses one completion channel and releases its waiter after close.
This spends thread resources to wait on independent pool addresses; the
scheduling measurements report the CPU, memory and latency costs. A wait cannot
interrupt a file operation already blocked on an operating-system lock.

### UDP sender and follower

UDP uses one explicit endpoint. The server must enable `--udp-follow`, which
allows unauthenticated, unencrypted reads on that socket. A send call reports
local validation or operating-system errors; it never returns a storage
receipt.

The [UDP receive policy](../../spec/udp/v1/SPEC.md#receive-ordering-and-notices)
requires arrival-order delivery, including late messages and duplicates. Only
explicit server skips produce gap events; sequence/progress jumps never suppress
frames or establish loss. This source change awaits a major release. Finite
follows end on terminal
progress and may leave DATA in flight.

```js
const { UdpClient, UdpStart } = require("plasmite");

(async () => {
  const client = new UdpClient("udp://127.0.0.1:9701");
  const pool = client.openPool("sensors");
  const follow = await pool.follow({ start: UdpStart.new(), live: true });

  try {
    pool.sendJson({ temperature: 23 }, ["sensor"]);
    for await (const event of follow) {
      if (event.kind === "frame") {
        console.log(event.seq, event.message.data);
        break;
      }
    }
  } finally {
    follow.close();
    pool.close();
    client.close();
  }
})().catch(console.error);
```

Use `UdpStart.last(count)`, `UdpStart.since(timestampNs)`, or
`UdpStart.sequence(seq)` to select retained history. Sequence and timestamp
metadata use `bigint`. Pass `bigint` for start values above JavaScript's safe
integer range; the SDK rejects unsafe `number` values instead of rounding them.
`follow()` returns a promise while the server confirms the initial subscription.
Closing its pool during that handshake does not cancel the opening call; the
call keeps its native handle until it succeeds or reaches the five-second
startup deadline. Once open, call `follow.close()` to close the subscription.
Await `nextEvent(timeoutMs)`; it returns `null` when the wait expires. Pass an
`AbortSignal` as the second argument to stop one pending wait. The async
iterator keeps waiting until a finite follow ends and accepts the same signal
in its options.

## Troubleshooting

- **Missing pool directory**: pool creation creates parent directories automatically. If you call `openPool(...)` on a missing pool, catch `ErrorKind.NotFound` or use `client.pool(...)` to create-or-open.
- **Permission denied**: choose a writable pool directory (`new Client("/path/to/pools")`) and verify directory permissions/ownership. Errors include `err.path` when available.

### Remote pools (HTTP/JSON)

This example uses a server's credential-free loopback HTTP listener on the
same machine. Create `events` and start `npx plasmite --dir ./data serve`
first. The JavaScript HTTP client works without the native addon.

It does not load connections saved by `access connect`, parse full `pk1`
access keys, or verify their certificate pins. Use Rust or the CLI for the
secure native access-key workflow. The optional `token`/`withToken` API sends
a supplied Bearer value only over HTTPS, except for loopback HTTP destinations
such as `localhost` and `127.0.0.1`. Requests without a token may use HTTP.
Redirects fail with an error and are never followed. HTTPS uses Node's normal
certificate checks and does not provide key pinning. Do not put a full access
key in the token field.

```js
const { RemoteClient } = require("plasmite");

(async () => {
  const client = new RemoteClient("http://127.0.0.1:9700");

  const pool = await client.openPool("events");

  // Append — accepts plain objects, serialized as JSON for you
  const message = await pool.append(
    { kind: "deploy", sha: "abc123" },
    ["ops"],
  );
  console.log(message.seq); // bigint sequence allocated by this pool

  // Read by sequence number
  const got = await pool.get(message.seq);
  console.log(got.data); // => { kind: "deploy", sha: "abc123" }

  // Tail — live-stream new messages (JSONL under the hood)
  for await (const msg of pool.tail({ sinceSeq: message.seq, tags: ["ops"], maxMessages: 1 })) {
    console.log(msg.seq, msg.tags, msg.data);
  }
})();
```

## API

### Local client

| Class | Method | Description |
|---|---|---|
| `Client(dir?)` | | Open a pool directory (`~/.plasmite/pools` by default) |
| | `.createPool(name, sizeBytes)` | Create a new pool (returns `Pool`) |
| | `.openPool(name)` | Open an existing pool (returns `Pool`) |
| | `.pool(name, sizeBytes?)` | Open if present, else create |
| | `.close()` | Release resources |
| | `[Symbol.dispose]()` | Alias for `.close()` (used by explicit resource-management syntax when enabled) |
| `Module` | `.parseMessage(value)` | Normalize a raw envelope or `Message` into a typed `Message` |
| `Pool` | `.appendJson(payload, tags, durability)` | Append JSON payload (Buffer or JSON-serializable value); returns message envelope as `Buffer` |
| | `.append(data, tags?, durability?)` | Append any JSON-serializable value; returns typed `Message` |
| | `.appendLite3(buf, durability?)` | Append raw bytes (lite3 framing); returns sequence `bigint` |
| | `.get(seq)` | Get message by sequence number; returns typed `Message` |
| | `.getJson(seq)` | Get message by sequence number; returns `Buffer` |
| | `.getLite3(seq)` | Get lite3 frame by sequence number |
| | `.tail(opts?)` | Async generator of typed `Message` values with optional tag filter |
| | `.tailViews<T>(opts?)` | Pull independent immutable snapshots; filter tags before the limit |
| | `.openStream(sinceSeq?, max?, timeoutMs?, errorOnGap?)` | Open a message stream |
| | `.openLite3Stream(sinceSeq?, max?, timeoutMs?, errorOnGap?)` | Open a lite3 frame stream |
| | `.close()` | Close the pool |
| | `[Symbol.dispose]()` | Alias for `.close()` (used by explicit resource-management syntax when enabled) |
| `Stream` | `.nextJson()` | Next message as `Buffer`, or `null` at end |
| | `[Symbol.iterator]()` | Iterate synchronously via `for...of` |
| | `.close()` | Close the stream |
| | `[Symbol.dispose]()` | Alias for `.close()` |
| `Lite3Stream` | `.next()` | Next lite3 frame, or `null` at end |
| | `[Symbol.iterator]()` | Iterate synchronously via `for...of` |
| | `.close()` | Close the stream |
| | `[Symbol.dispose]()` | Alias for `.close()` |
| `UdpClient(endpoint)` | | Open one explicit UDP server endpoint |
| | `.openPool(name)` | Open a named pool handle |
| `UdpPool` | `.sendJson(data, tags?)` | Send a JSON-origin message without a storage receipt |
| | `.sendLite3(buf)` | Send a complete Lite3 document |
| | `.follow(opts?)` | Open a best-effort UDP event stream (`Promise<UdpFollow>`) |
| | `.followViews<T>(opts?)` | Open the same event stream with owned message views |
| `UdpFollow` | `.nextEvent(timeoutMs?)` | Read one frame, gap, or progress event; return `null` at timeout or end |
| | `[Symbol.asyncIterator]()` | Iterate events asynchronously |
| | `.close()` | Cancel a wait and close the subscription |

**Durability** controls fsync behavior:
- `Durability.Fast` — buffered writes (higher throughput)
- `Durability.Flush` — fsync after write (crash-safe)

Sequence numbers accept `number` or `bigint`; use `bigint` when they exceed
JavaScript's safe-integer range. `sinceSeq` is inclusive: to resume after the
last handled message, save `message.seq + 1n` as the next checkpoint.

Local tails continue from the next retained message by default when their
cursor has fallen behind the ring buffer. Set `errorOnGap: true` when losing
messages must stop the consumer:

```js
for await (const message of pool.tail({
  sinceSeq: checkpoint,
  errorOnGap: true,
})) {
  await handle(message);
}
```

The tail throws `PlasmiteNativeError` with
`kind === ErrorKind.RetentionGap` before delivering a message after the gap.
Persist the next sequence to read (`message.seq + 1n`) after handling each
message, and restart from a suitable checkpoint after deciding how to recover.

### Owned HTTP messages

`RemotePool` offers the same immutable data interface without a native addon:

```js
const { RemoteClient, toLite3 } = require("plasmite");
const remote = await new RemoteClient("http://127.0.0.1:9700").openPool("sensors");
const sent = await remote.appendView({ counter: 9223372036854775807n }, ["sensor"]);
const message = await remote.getView(sent.seq);
console.log(message.data.counter); // bigint, with exact digits
const detached = toLite3(message); // Mutable copy, without sequence/timestamp.
for await (const item of remote.tailViews({ sinceSeq: sent.seq, maxMessages: 1 })) {
  console.log(item.data.counter);
}
```

The package bundles a checked WebAssembly core for HTTP encoding and validation.
Its first use compiles that module; later calls reuse it. The input walk runs
synchronously before network I/O, so getters run once and later mutations cannot
change the submitted payload. HTTP transfers and owner acquisition still copy
bytes. Native local calls and HTTP calls use different backends; neither exposes
mutable storage through a view. Retaining a child retains the whole snapshot.

Binary HTTP reads require current-server identity headers. `tailViews` rejects
`errorOnGap: true`, like `tailLite3`: binary frames cannot carry terminal errors.
EOF does not certify complete delivery after a failure once streaming starts.
Use the JSON `tail` if you require its fail-closed retention-gap reporting.
An `AbortSignal` cancels an HTTP read; breaking a tail closes its response.

### Remote client

| Class | Method | Description |
|---|---|---|
| `RemoteClient(url, opts?)` | | Connect to a plasmite server |
| | `.withToken(token)` | Set bearer token (returns `this`) |
| | `.createPool(name, sizeBytes)` | Create a pool on the server |
| | `.openPool(name)` | Open a remote pool (returns `RemotePool`) |
| | `.poolInfo(name)` | Get pool metadata |
| | `.listPools()` | List all pools |
| | `.deletePool(name)` | Delete a pool |
| `RemotePool` | `.append(data, tags?, durability?)` | Append a message (data is any JSON-serializable value) |
| | `.get(seq)` | Get a message by sequence number |
| | `.getView(seq)` / `.appendView(data, tags?)` | Read or produce immutable encoded messages |
| | `.tailViews(opts?)` | Pull immutable messages over binary HTTP |
| | `.tail(opts?)` | Live-tail typed messages (`AsyncGenerator<Message>`) |

### Tail options

```js
const options = {
  sinceSeq: 1n,        // start at this sequence number (inclusive)
  maxMessages: 100,    // stop after N messages
  timeoutMs: 5000,     // stream timeout; see local/remote distinction below
  tags: ["signup"],    // filter by exact tag match (AND across tags)
  errorOnGap: true,    // fail before delivering a message after a retention gap
};
```

Local `tail()` retains one native reader and waits on worker threads, so timers
and other JavaScript work keep running. `timeoutMs` controls each wait, not a
total deadline. With `maxMessages`, a quiet wait also ends the tail. Pass
`signal: controller.signal` to cancel a quiet tail through an `AbortController`.
Matching-message limits apply after tag filtering. Low-level streams offer
`nextJsonWait(timeoutMs)` or `nextWait(timeoutMs)` with statuses 0 (wait expired),
1 (message/frame), 2 (finished), and 3 (cancelled). Their `cancel()` and `close()`
methods interrupt active waits; worker handles stay alive until the calls finish. Remote `tail()` makes one request and
ends when that server stream closes or times out. Breaking out of either async iterator closes its stream.

Remote JSON tails surface a fail-closed gap as `RemoteError` with
`kind === ErrorKind.RetentionGap`. Remote Lite3 tails do not support this
policy because their frame format has no terminal error representation.

### Error handling

Local operations throw `PlasmiteNativeError` with structured fields. Optional
`seq` and `offset` use `bigint`, as do the corresponding `RemoteError` fields:

```js
try {
  pool.get(999n);
} catch (err) {
  if (err instanceof PlasmiteNativeError) {
    console.log(err.kind === ErrorKind.NotFound); // true
    console.log(err.seq);    // 999
  }
}
```

HTTP error responses throw `RemoteError` with `status`, `kind`, and optional
`hint`. Connection and TLS failures may throw Node fetch errors, and malformed
response bodies may throw parse errors; do not assume every remote failure
is an HTTP error response.

### CLI

The package includes the `plasmite` CLI:

```bash
npx plasmite --version
npx plasmite --dir ./data serve --bind 127.0.0.1:9700
```

## TypeScript

Type declarations ship with the package (`types.d.ts`). No `@types/` install
needed.

```ts
import { Client, Durability, Pool, RemoteClient } from "plasmite";
```

For repo development, `npm test` runs:
- native rebuild
- `npm run check:type-surface` (runtime export ↔ `types.d.ts` drift check)
- `node --test test/*.test.js` (binding tests)

## Platform support

Pre-built binaries are included for:
- Linux: `x64`
- macOS: `x64`, `arm64`
- Windows: `x64`

Linux ARM64 and ARMv7 have [preview SDK archives](https://github.com/sandover/plasmite/blob/main/docs/record/distribution.md#linux-arm-sdk-preview-install)
for the native CLI/server, but no published npm addon or CLI. Installing an
SDK archive does not supply a Node addon. The HTTP `RemoteClient` remains
available without native binaries, including owned views through the bundled
WebAssembly backend. This Node entry point uses Node file and Buffer APIs; it
does not provide a browser package.

For source development, build the addon and stage its native assets with
`npm run build` and `npm run prepare-native` on a compatible host.
See the [distribution guide](https://github.com/sandover/plasmite/blob/main/docs/record/distribution.md) for channel
support and the [1.0 upgrade guide](https://github.com/sandover/plasmite/blob/main/docs/record/upgrading-1.0.md) for CLI
and remote-access changes.

## License

MIT

For playback timing, use the asynchronous tail and an application scheduler; see the
[next major upgrade guide](../../docs/record/upgrading-next-major.md#sdk-applications-control-playback-timing).

## Numeric append receipts

```js
const receipt = pool.appendIdentity({ temperature: 23.5 }, ["sensor"]);
console.log(receipt.seq, receipt.timestampNs); // exact bigint values
```

`appendLite3Identity` accepts encoded Lite3 bytes. These local methods return
only the committed sequence and Unix nanoseconds, without echoing or decoding
the data. Existing `append` returns a full Message. Use the native library from
the matching SDK. Validation, durability and commit ordering stay the same.
