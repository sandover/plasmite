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

### Local pools

```js
const { Client } = require("plasmite");

async function main() {
  const client = new Client("./data");
  const pool = client.openPool("events", { create: true });
  try {
    const live = await pool.follow({ tags: ["sensor"] });
    try {
      const stored = await pool.feed({ temperature: 23 }, { tags: ["sensor"] });
      const { value: received } = await live.next();
      console.log(received.seq, received.time, received.data);
      for await (const message of await pool.fetch({ since: stored.seq })) {
        console.log(message.data);
      }
      console.log(pool.info().bounds);
    } finally {
      await live.return();
    }
  } finally {
    pool.close();
    client.close();
  }
}

main().catch(console.error);
```

`follow()` opens on new messages. `fetch()` reads retained history and ends
at the last sequence present when it opened. Both readers take `since`,
`last`, `tags`, and `signal`. Sequence and Unix nanosecond time use `bigint`.

### Owned views

`fetchView` and `followView` yield immutable Lite3 snapshots. Properties read
the stored data. Arrays support indexing, iteration and shallow methods.
Children remain valid after pool close or ring overwrite.

```js
const { toNative, toJson, toLite3 } = require("plasmite");
const reader = await pool.fetchView({ since: 17n });
const { value: message } = await reader.next();
await reader.return();
const metrics = message.data.metrics;
console.log(metrics.power);
for (const sample of message.data.samples) console.log(sample.value);
const editable = toNative(metrics); // Independent mutable subtree.
editable.power = 42;
const json = toJson(message);       // Direct formatting, exact integer digits.
const encoded = toLite3(message);   // Detached mutable Buffer; identity stays separate.
```

Use `pool.fetchView<Reading>({ since: 17n })` in TypeScript to declare an expected data shape;
this assertion performs no runtime validation. `toNative(message)` creates the
canonical envelope with a string time; subtree conversion preserves its shape.
Safe stored integers read as numbers and larger i64 values as bigints. Use
`toJson` for exact JSON; ordinary JSON.stringify can reject bigint data.

Object views have no Object prototype methods; use Object.hasOwn. Spread and
slice make shallow containers whose children remain views. Mutation, freeze and
seal throw TypeError; structuredClone cannot clone these proxies. Native copies
preserve __proto__ as an own data property.

Views avoid full decoding for selected reads, but property access still crosses
a language boundary. Native conversion and small-record latency have different
costs from legacy methods. See the
[measured comparisons](../../docs/performance/language-boundary-results/node-views/README.md).
Run `node --expose-gc scripts/bench_views.js 100 5 result.json` from this directory
after staging matching optimized native assets to repeat the focused comparison.

### Owned local streams

```js
const controller = new AbortController();
for await (const message of await pool.followView({
  tags: ["sensor"], since: 1n, signal: controller.signal,
})) {
  console.log(message.data.metrics.power);
  break; // Closes this reader; retained messages remain readable.
}
```

`followView` pulls one snapshot at a time. It uses the same selection, gap
handling and per-call wait as `follow`. Abort the signal when abandoning a
pending read. Breaking the loop also closes it.
UDP `await udpPool.followView(options)` uses the same follow lifecycle as
`follow`; frame events carry the view in `event.message`. Gap and progress
events keep their existing shape. Close the follow in `finally`.

### Native input and committed views

```js
const sent = await pool.feedView({
  metrics: { power: 42 }, bytes: Buffer.from([1, 2]), counter: 9223372036854775807n,
}, { tags: ["sensor"] });
const forwarded = await destination.feedView(sent); // Reuse its encoding and tags.
const retagged = await destination.feedView(sent.data, { tags: ["processed"] });
```

`feedView` evaluates input once on the calling thread, queues storage work,
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

Data must encode as an object. A message view forwards its data and existing
tags. Pass the view's `data` to supply new tags. A payload subtree copies directly through the native
writer, including exact integers and bytes. TypeScript's optional
`feedView<Reading>(input)` type argument asserts the output shape without
runtime validation.

### Owned UDP streams

```js
const follow = await udpPool.followView({ tags: ["sensor"] });
try {
  for await (const event of follow) {
    if (event.kind === "frame") console.log(event.message.data.metrics.power);
  }
} finally { follow.close(); }
```

Retained children survive iterator cleanup, pool overwrite and handle close.
UDP frame events contain `message`, sequence and nanosecond time. They omit the
separate mutable payload Buffer. Gap and progress events keep their existing
meaning and fields. Each iterator reads on demand and creates no default
prefetch queue. A supplied type argument asserts the payload shape.

Passing a signal to UDP `nextEvent` cancels that wait and keeps the reader open. Leaving an async iteration, aborting a signal supplied to `follow`, `fetch` or `events`, and calling `cancel()` or `close()` all release the subscription after any active call finishes. The view variants follow the same rules.

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
frames or establish loss. Finite readers end on terminal progress and may
leave DATA in flight.

```js
const { UdpClient } = require("plasmite");

(async () => {
  const client = new UdpClient("udp://127.0.0.1:9701");
  const pool = client.openPool("sensors");
  const follow = await pool.follow();

  try {
    await pool.feed({ temperature: 23 }, { tags: ["sensor"] });
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

Use `last`, `since: at(timestampNs)`, or `since: sequence` to select retained
history. Use `fetch` to stop at the server's opening boundary. Sequence and timestamp
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

- **Missing pool directory**. Pass `{ create: true }` to `openPool` to create a missing pool and its parent directories.
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
  const pool = await client.openPool("events", { create: true });
  const saved = await pool.feed({ kind: "deploy", sha: "abc123" }, { tags: ["ops"] });
  const retained = await pool.fetch({ since: saved.seq, tags: ["ops"] });
  for await (const message of retained) console.log(message.data);
  console.log((await pool.info()).bounds);
})().catch(console.error);
```

## API

### Local client

| Class | Method | Description |
|---|---|---|
| `Client(dir?)` | | Open a pool directory (`~/.plasmite/pools` by default) |
| | `.createPool(name, sizeBytes)` | Create a new pool (returns `Pool`) |
| | `.openPool(name, { create })` | Open a pool, optionally creating it at the default size |
| | `.listPools()` and `.deletePool(name)` | Inspect the directory or delete a pool |
| | `.close()` | Release resources |
| | `[Symbol.dispose]()` | Alias for `.close()` (used by explicit resource-management syntax when enabled) |
| `Module` | `.parseMessage(value)` | Normalize a raw envelope or `Message` into a typed `Message` |
| `Pool` | `.info()` | Inspect retained history and file size |
| | `.feed(data, { tags, durability })` | Store an object and return the committed message |
| | `.feedJson(buffer, options)` | Store a JSON object and return the encoded envelope |
| | `.follow(options)` | Open an async reader on new messages by default |
| | `.fetch(options)` | Open an async reader on finite retained history |
| | `.followView(options)` and `.fetchView(options)` | Read immutable message views |
| | `.close()` | Close the pool |
| | `[Symbol.dispose]()` | Alias for `.close()` (used by explicit resource-management syntax when enabled) |
| `Reader` | `.next(waitSeconds?)` | Pull the next item, optionally bounding this wait |
| | `.return()` | Close the reader and cancel a pending wait |
| `UdpClient(endpoint)` | | Open one explicit UDP server endpoint |
| | `.openPool(name)` | Open a named pool handle |
| `UdpPool` | `.feed(data, options)` | Send a message without a storage receipt |
| | `.feedLite3(buffer)` | Send a complete Lite3 document |
| | `.follow(options)` and `.fetch(options)` | Open a best-effort UDP event stream |
| | `.followView(options)` and `.fetchView(options)` | Open a UDP stream with owned message views |
| `UdpFollow` | `.nextEvent(timeoutMs?)` | Read one frame, gap, or progress event; return `null` at timeout or end |
| | `[Symbol.asyncIterator]()` | Iterate events asynchronously |
| | `.close()` | Cancel a wait and close the subscription |

**Durability** controls fsync behavior:
- `Durability.Fast` — buffered writes (higher throughput)
- `Durability.Flush` — fsync after write (crash-safe)

Sequence numbers accept `number` or `bigint`; use `bigint` when they exceed
JavaScript's safe-integer range. `since` is inclusive. To resume after the
last handled message, save `message.seq + 1n` as the next checkpoint.

Read from a saved checkpoint as follows.

```js
for await (const message of await pool.follow({ since: checkpoint })) {
  await handle(message);
}
```

If the pool overwrites unread messages, the reader throws `PlasmiteNativeError`
with `kind === ErrorKind.RetentionGap`, `missing`, and `nextSeq`.
Persist the next sequence to read (`message.seq + 1n`) after handling each
message, and restart from a suitable checkpoint after deciding how to recover.

### Owned HTTP messages

Use `feedView`, `fetchView`, and `followView` to work with immutable views
over HTTP without building a native JavaScript object tree. The package checks
and reads Lite3 with its bundled WebAssembly module. The module loads on first
use. A retained child keeps the complete message alive.

Use `feedLite3`, `fetchLite3`, and `followLite3` for encoded Lite3 bytes and
exact message identity. Both JSON and binary streams report retention gaps
with `missing` and `nextSeq`. Leaving the loop or aborting its signal closes
the HTTP response.

### Remote client

| Operation | Method |
| --- | --- |
| Create | `await client.createPool(name, sizeBytes)` |
| Open | `await client.openPool(name, { create })` |
| Inspect | `await pool.info()` |
| List | `await client.listPools()` |
| Delete | `await client.deletePool(name)` |
| Send | `await pool.feed(data, { tags, durability })` |
| Follow | `await pool.follow({ since, last, tags, signal })` |
| Fetch | `await pool.fetch({ since, last, tags, signal })` |

### Read options

Pass either `since` or `last`. An inclusive sequence such as
`since: 42n` resumes at that record. Use `since: "oldest"` to read from the
first retained record, `since: "5m"` for a relative start, or
`since: at(epochNanoseconds)` for an exact instant. A message must carry every
requested tag.

`await reader.next(2)` waits up to two seconds. On expiry it returns
`{ done: false, value: undefined }`. A finished fetch returns
`{ done: true, value: undefined }`. To cancel a pending read, abort its signal
or call `await reader.return()`.

### Error handling

Local operations throw `PlasmiteNativeError` with structured fields. Optional
`seq` and `offset` use `bigint`, as do the corresponding `RemoteError` fields:

```js
try {
  await pool.fetch({ since: checkpoint + 1n });
} catch (err) {
  if (err instanceof PlasmiteNativeError) {
    if (err.kind === ErrorKind.RetentionGap) console.log(err.missing, err.nextSeq);
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

For playback timing, use the asynchronous reader and an application scheduler; see the
[2.0 upgrade guide](../../docs/record/upgrading-2.0.md#applications-control-playback-timing).

## HTTPS pools

After saving a connection with `plasmite access connect https://pools.example.net:9743`, use the same module functions for a pool URL.

```js
const pool = openPool("https://pools.example.net:9743/events", { create: true });
try {
  await pool.feed({ ready: true }, { tags: ["status"] });
  for await (const message of await pool.fetch({ last: 1 })) console.log(message.data);
} finally {
  pool.close();
}
```

Use `reader.next(wait)` with seconds on any transport. A zero wait polls. Use an `AbortSignal` or close the reader to stop it. The separate `RemoteClient` also works without a native addon on a remote-only installation.

## Numeric feed receipts

```js
const receipt = await pool.feedReceipt({ temperature: 23.5 }, { tags: ["sensor"] });
console.log(receipt.seq, receipt.time); // exact bigint values
```

`feedJsonReceipt` accepts JSON bytes and `feedLite3Receipt` accepts encoded
Lite3 bytes. Both resolve to the committed sequence and time. All feed methods on a native pool return promises.
