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

## Troubleshooting

- **Missing pool directory**: pool creation creates parent directories automatically. If you call `openPool(...)` on a missing pool, catch `ErrorKind.NotFound` or use `client.pool(...)` to create-or-open.
- **Permission denied**: choose a writable pool directory (`new Client("/path/to/pools")`) and verify directory permissions/ownership. Errors include `err.path` when available.

### Remote pools (HTTP/JSON)

Connect to a Plasmite server (`npx plasmite serve` or `pls serve`) to read
and write pools over the network.

```js
const { RemoteClient } = require("plasmite");

(async () => {
  const client = new RemoteClient("http://127.0.0.1:9700");
  // With auth: new RemoteClient("http://...", { token: "secret" })

  const pool = await client.openPool("events");

  // Append — accepts plain objects, serialized as JSON for you
  const message = await pool.append(
    { kind: "deploy", sha: "abc123" },
    ["ops"],
  );
  console.log(message.seq); // => 1n

  // Read by sequence number
  const got = await pool.get(1);
  console.log(got.data); // => { kind: "deploy", sha: "abc123" }

  // Tail — live-stream new messages (JSONL under the hood)
  for await (const msg of pool.tail({ sinceSeq: 0, tags: ["ops"], maxMessages: 1 })) {
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
| | `.replay(pool, opts?)` | Backward-compatible replay wrapper (delegates to `pool.replay`) |
| `Pool` | `.appendJson(payload, tags, durability)` | Append JSON payload (Buffer or JSON-serializable value); returns message envelope as `Buffer` |
| | `.append(data, tags?, durability?)` | Append any JSON-serializable value; returns typed `Message` |
| | `.appendLite3(buf, durability?)` | Append raw bytes (lite3 framing); returns sequence `bigint` |
| | `.get(seq)` | Get message by sequence number; returns typed `Message` |
| | `.getJson(seq)` | Get message by sequence number; returns `Buffer` |
| | `.getLite3(seq)` | Get lite3 frame by sequence number |
| | `.tail(opts?)` | Async generator of typed `Message` values with optional tag filter |
| | `.replay(opts?)` | Async generator of typed `Message` values with speed/timing controls |
| | `.openStream(sinceSeq?, max?, timeoutMs?)` | Open a message stream |
| | `.openLite3Stream(sinceSeq?, max?, timeoutMs?)` | Open a lite3 frame stream |
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
| | `.tail(opts?)` | Live-tail typed messages (`AsyncGenerator<Message>`) |

### Tail options

```js
const options = {
  sinceSeq: 0,         // start after this sequence number
  maxMessages: 100,    // stop after N messages
  timeoutMs: 5000,     // stop after N ms of inactivity
  tags: ["signup"],    // filter by exact tag match (AND across tags)
};
```

### Error handling

Local operations throw `PlasmiteNativeError` with structured fields:

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

Remote operations throw `RemoteError` with `status`, `kind`, and optional
`hint`.

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
- `node --test test/*.test.js` (includes conformance tests)

Run the conformance runner directly:

```bash
cd bindings/node
npm run build
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" \
PLASMITE_BIN="$(pwd)/../../target/debug/plasmite" \
node cmd/plasmite-conformance.js ../../conformance/sample-v0.json
```

## Platform support

Pre-built binaries are included for:
- Linux: `x64`, `arm64`
- macOS: `x64`, `arm64`
- Windows: `x64`

If your platform/architecture is not listed, install the SDK (`libplasmite`) and build from source.

## License

MIT
