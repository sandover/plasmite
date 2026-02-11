# plasmite

Persistent JSON message queues for Node.js. No broker, no daemon — just files on disk.

Plasmite gives you fast, crash-safe, disk-backed ring buffers ("pools") that
multiple processes can read and write concurrently. Use it for IPC, event
sourcing, job queues, or anywhere you'd reach for Redis but don't want to run
a server.

## Install

```bash
npm install plasmite
```

Requires Node 20+. The package ships pre-built native binaries — no Rust
toolchain or compile step needed.

## Quick start

### Local pools (native, in-process)

```js
const { Client, Durability } = require("plasmite");

// Open a client pointed at a directory (created if it doesn't exist)
const client = new Client("./data");

// Create a 64 MB pool called "events"
const pool = client.createPool("events", 64 * 1024 * 1024);

// Append a JSON message with tags
pool.appendJson(
  Buffer.from(JSON.stringify({ kind: "signup", user: "alice" })),
  ["user-event"],
  Durability.Flush,
);

// Read it back by sequence number
const msg = pool.getJson(1n);
console.log(JSON.parse(msg.toString()));
// => { kind: "signup", user: "alice" }

// Stream messages as they arrive
const stream = pool.openStream();
let frame;
while ((frame = stream.nextJson()) !== null) {
  console.log(JSON.parse(frame.toString()));
}
stream.close();

pool.close();
client.close();
```

### Remote pools (HTTP/JSON)

Connect to a plasmite server (`npx plasmite serve` or `pls serve`) to read
and write pools over the network.

```js
const { RemoteClient } = require("plasmite");

const client = new RemoteClient("http://127.0.0.1:9700");
// With auth: new RemoteClient("http://...", { token: "secret" })

const pool = await client.openPool("events");

// Append — accepts plain objects, serialized as JSON for you
const message = await pool.append(
  { kind: "deploy", sha: "abc123" },
  ["ops"],
);
console.log(message.seq); // => 1

// Read by sequence number
const got = await pool.get(1);
console.log(got.data); // => { kind: "deploy", sha: "abc123" }

// Tail — live-stream new messages (JSONL under the hood)
const tail = await pool.tail({ sinceSeq: 0, tags: ["ops"] });
const next = await tail.next(); // resolves on next matching message
console.log(next);
tail.cancel();
```

## API

### Local client

| Class | Method | Description |
|---|---|---|
| `Client(dir)` | | Open a pool directory |
| | `.createPool(name, sizeBytes)` | Create a new pool (returns `Pool`) |
| | `.openPool(name)` | Open an existing pool (returns `Pool`) |
| | `.close()` | Release resources |
| `Pool` | `.appendJson(buf, tags, durability)` | Append a JSON message; returns the stored envelope as `Buffer` |
| | `.appendLite3(buf, durability)` | Append raw bytes (lite3 framing); returns sequence `bigint` |
| | `.getJson(seq)` | Get message by sequence number; returns `Buffer` |
| | `.getLite3(seq)` | Get lite3 frame by sequence number |
| | `.openStream(sinceSeq?, max?, timeoutMs?)` | Open a message stream |
| | `.openLite3Stream(sinceSeq?, max?, timeoutMs?)` | Open a lite3 frame stream |
| | `.close()` | Close the pool |
| `Stream` | `.nextJson()` | Next message as `Buffer`, or `null` at end |
| | `.close()` | Close the stream |

**Durability** controls fsync behavior:
- `Durability.Fast` — buffered writes (higher throughput)
- `Durability.Flush` — fsync after write (crash-safe)

Sequence numbers accept `number` or `bigint`.

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
| | `.tail(opts?)` | Live-tail messages (returns `RemoteTail`) |
| `RemoteTail` | `.next()` | Await next message (resolves to message or `null`) |
| | `.cancel()` | Stop the tail stream |

### Tail options

```js
await pool.tail({
  sinceSeq: 0,         // start after this sequence number
  maxMessages: 100,    // stop after N messages
  timeoutMs: 5000,     // stop after N ms of inactivity
  tags: ["signup"],    // filter by exact tag match (AND across tags)
});
```

### Error handling

Local operations throw `PlasmiteNativeError` with structured fields:

```js
try {
  pool.getJson(999n);
} catch (err) {
  if (err instanceof PlasmiteNativeError) {
    console.log(err.kind);   // "NotFound"
    console.log(err.seq);    // 999
  }
}
```

Remote operations throw `RemoteError` with `status`, `kind`, and optional
`hint`.

### CLI

The package includes the `plasmite` CLI:

```bash
npx plasmite --version
npx plasmite serve ./data --port 9700
```

## TypeScript

Type declarations ship with the package (`types.d.ts`). No `@types/` install
needed.

```ts
import { Client, Durability, Pool, RemoteClient } from "plasmite";
```

## Platform support

Pre-built binaries are included for Linux x86_64. macOS and Windows users
should install via Homebrew (`brew install sandover/tap/plasmite`) or build
from source.

## Contributing

Development requires a Rust toolchain to build the native addon:

```bash
# From the repo root
cargo build -p plasmite
cd bindings/node && PLASMITE_LIB_DIR=../../target/debug npm test
```

See the [main repo](https://github.com/sandover/plasmite) for full build
instructions.

## License

MIT
