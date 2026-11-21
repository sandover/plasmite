# Plasmite for Node.js

Plasmite stores JSON messages in pools: bounded ring buffers on disk that many processes can write and read at once. `feed` writes a message, `follow` waits for new ones, and `fetch` reads what the pool holds and stops. Each message carries a sequence number (`seq`), a time, tags and a JSON object as `data`. When a pool fills, the oldest messages make room for new ones. This package opens local pools, pools on a shared HTTPS server and UDP endpoints. The [project README](https://github.com/sandover/plasmite#readme) explains the model and the CLI.

## Install

```bash
npm install plasmite
```

The package needs Node.js 24 or newer. It includes native binaries and the `plasmite` CLI (`npx plasmite`) for macOS (x64, arm64), Linux x64 with glibc 2.35 or newer, and Windows x64. The HTTP `RemoteClient` runs without those binaries. TypeScript declarations ship with the package. See [distribution](https://github.com/sandover/plasmite/blob/main/docs/record/distribution.md).

## Quick start

Record a temperature reading, watch for it, then read recent history. Save this as `sensors.mjs`:

```js
import { Client } from "plasmite";

const client = new Client("./data");
const pool = client.openPool("sensors", { create: true });

const live = await pool.follow({ tags: ["kitchen"] });
await pool.feed({ temp_c: 21.4 }, { tags: ["kitchen"] });
console.log((await live.next()).value.data.temp_c);   // 21.4
await live.return();

for await (const message of await pool.fetch({ last: 10 })) {
  console.log(message.seq, [...message.tags], message.data.temp_c);
}
pool.close();
client.close();
```

`follow` fixes its position before its promise resolves, so it sees the feed that comes after it. `fetch` stops at the last message present when it opened. `seq` and `time` are `bigint`s; `time` counts Unix nanoseconds. `pool.info().bounds` reports the oldest and newest retained sequence. The module functions `openPool`, `createPool`, `listPools` and `deletePool` use the default directory, `~/.plasmite/pools`. Use `createPool(ref, { sizeBytes })` to choose a size; `create: true` makes a 1 MiB pool.

## Reading

Every read starts at a position. `follow` starts at the next new message by default; `fetch` starts at the oldest retained one.

```js
import { at } from "plasmite";

await pool.fetch({ since: "oldest" });               // everything the pool still holds
await pool.fetch({ since: 42n });                    // sequence 42 onward, inclusive
await pool.fetch({ since: "5m" });                   // the last five minutes
await pool.fetch({ since: "2026-11-19T09:00:00Z" }); // an RFC 3339 time
await pool.fetch({ since: at(1795078800000000000n) }); // the same time as Unix nanoseconds
await pool.fetch({ last: 20 });                      // the 20 most recent messages
await pool.follow({ tags: ["error"] });              // only messages carrying every listed tag
```

`follow` takes the same options. Pass `since` or `last`, not both. Sequences accept `number` or `bigint`; use `bigint` above `Number.MAX_SAFE_INTEGER`.

Resume after a restart by saving the next sequence to read. If the pool overwrote messages before you read them, opening or advancing the reader can throw a `RetentionGap` error.

```js
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { ErrorKind } from "plasmite";

const checkpoint = "sensors.seq";
let start = existsSync(checkpoint) ? BigInt(readFileSync(checkpoint, "utf8")) : "oldest";
for (;;) {
  try {
    for await (const message of await pool.follow({ since: start })) {
      console.log(message.data.temp_c);
      writeFileSync(checkpoint, String(message.seq + 1n));
    }
  } catch (err) {
    if (err.kind !== ErrorKind.RetentionGap) throw err;
    console.log(`lost ${err.missing}..${err.nextSeq - 1n}; resuming at ${err.nextSeq}`);
    start = err.nextSeq;
    writeFileSync(checkpoint, String(start));
  }
}
```

`err.missing` is the first sequence you missed and `err.nextSeq` the oldest one still retained. Saving `nextSeq` accepts the loss and moves on.

To wait for a bounded time, call `await reader.next(seconds)`. It resolves to `{ done: false, value: undefined }` when the wait expires and leaves the reader open; `reader.next(0)` polls. To stop a reader, leave the loop, call `reader.return()`, or pass `signal` from an `AbortController` when you open it.

## HTTPS pools and UDP

To use a pool on a shared server, connect once with `plasmite access connect https://pools.example.net:9743`, then open the pool by URL. `listPools()` still lists the local directory.

```js
import { openPool } from "plasmite";

const pool = openPool("https://pools.example.net:9743/sensors", { create: true });
await pool.feed({ temp_c: 21.4 }, { tags: ["kitchen"] });
pool.close();
```

`RemoteClient` is a separate HTTP client written in JavaScript. Construct it with a base URL and optional `{ token }` bearer credentials. Its asynchronous `openPool` returns pools with the same `feed`, `follow`, `fetch`, `info` and `setDescription` calls. It uses Node's normal certificate checks. For the credentials and certificate pins saved by `plasmite access connect`, use the native `openPool` shown here. The HTTP client sends bearer tokens only over HTTPS or to loopback addresses.

UDP trades delivery guarantees for low overhead. A send resolves once the operating system accepts the datagram. Reads deliver frames in arrival order, including duplicates and late arrivals. Before running this example, create `sensors` with `plasmite pool create sensors` and run `plasmite serve --udp-follow` in another terminal.

```js
import { UdpClient } from "plasmite";

const client = new UdpClient("udp://127.0.0.1:9701");
const pool = client.openPool("sensors");
await pool.feed({ temp_c: 21.4 }, { tags: ["kitchen"] });
for await (const message of await pool.follow({ since: "oldest" })) {
  console.log(message.seq, message.data.temp_c);
}
```

Use `reader.events()` to see `skip` ranges and `progress` alongside received messages. `reader.nextEvent(timeoutMs)` waits in milliseconds and returns `null` when the wait expires. `reader.next(seconds)` yields messages and reports a retention skip as `RetentionGap`. Call `reader.close()` when you finish. The [UDP spec](https://github.com/sandover/plasmite/blob/main/spec/udp/v1/SPEC.md) defines delivery and limits.

## Errors and concurrency

Native calls throw `PlasmiteNativeError`; `RemoteClient` throws `RemoteError`, which also carries the HTTP `status` and an optional `hint`. Both carry `kind`, one of the nine `ErrorKind` values: `Usage`, `NotFound`, `AlreadyExists`, `Busy`, `Permission`, `Corrupt`, `Io`, `RetentionGap` and `Internal`. They also carry `path`, `seq` and `offset` when known, and `missing` and `nextSeq` for a retention gap; numeric fields are `bigint`s. Bad read options throw with kind `Usage`. Network and TLS failures from `RemoteClient` can surface as ordinary `fetch` errors.

Feeds and reads return promises. The native add-on runs pool writes and reader waits on worker threads. Open one reader per consumer. A second concurrent `next()` on the same native pool reader throws `Busy`. The HTTP-only reader reports `Io` in that case.

## Messages

The local, HTTP and UDP readers yield the same immutable `Message`. Local and HTTP `feed` return the stored message. UDP `feed` returns once it sends the datagram.

Read `seq`, `time`, `tags` and the optional `sender` without decoding `data`. Use `message.field(name)` to read one payload field directly. It returns `undefined` if the key is missing. The message and every field you keep stay usable after the pool overwrites the record or the reader closes. Keyed server writes expose the access key name in `sender`.

```js
import { toJson, toNative } from "plasmite";

const reader = await pool.fetch({ last: 1 });
const { value: message } = await reader.next();
await reader.return();

console.log(message.seq, message.field("temp_c"));
const editable = toNative(message.data);
console.log(toJson(message));
await pool.feed(message);
await pool.feed(message.raw, { format: "lite3" });
await pool.feed(Buffer.from('{"temp_c":21.4}'), { format: "json", tags: ["kitchen"] });
```

Forwarding a `Message` or its Lite3 bytes keeps the data and tags and assigns a fresh sequence and time. To change the tags, feed `message.data` with the new tags. Each access to `message.raw` copies the stored Lite3 bytes into a Buffer you can edit.

`message.data` decodes the payload once and caches an ordinary JavaScript object. Edits to that object leave the stored message, forwarded bytes and fields from `message.field` intact. Objects and arrays from `message.field` are read-only. `toNative` copies a message or field into ordinary mutable containers. Use `toJson` to format a message with exact integer digits. Plasmite reads larger stored integers as `bigint` and accepts `Buffer` or `Uint8Array` byte values inside input objects. In TypeScript, `fetch<Reading>()` specifies the expected payload shape.

## Pool descriptions

Give a pool a short description and an example so readers can choose it and see what to write.

```js
const sensors = client.createPool("sensors", {
  about: "Kitchen temperature readings",
  example: { temp_c: 21.4 },
});

console.log(sensors.info().about);
sensors.setDescription({ about: "Upstairs temperature readings", example: { temp_c: 20.1 } });
```

The HTTP client takes the same options. Its `createPool`, `info` and `setDescription` calls return promises. `setDescription` replaces both fields. Omit either field to clear it.

## Building from source

From the repo root, build and stage the native library and run the binding tests.

```bash
just bindings-node-test
```

Inside `bindings/node`, `npm run build` compiles the add-on and `npm run prepare-native` stages its native library. `npm test` runs both, checks that `types.d.ts` matches the runtime exports, and runs the tests in `test/`.

## License

MIT
