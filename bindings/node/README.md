<!--
Purpose: Document how to build and use the Plasmite Node bindings.
Exports: N/A (documentation).
Role: Quickstart for Node/TypeScript users of libplasmite.
Invariants: Uses the C ABI via N-API and matches v0 semantics.
Notes: Requires libplasmite to be built and discoverable.
-->

# Plasmite Node Bindings (v0)

These bindings wrap the `libplasmite` C ABI via a N-API addon.

## Build Requirements

- Node 20+
- Rust toolchain (for building the addon)
- `libplasmite` built from this repo (`cargo build -p plasmite`)

## Build & Test

From the repo root:

```bash
cargo build -p plasmite
```

Then from `bindings/node`:

```bash
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" npm test
```

On macOS, ensure `DYLD_LIBRARY_PATH` includes the same directory.
On Linux, set `LD_LIBRARY_PATH`.

## Usage

```js
const { Client, Durability } = require("plasmite-node")

const client = new Client("./data")
const pool = client.createPool("docs", 64 * 1024 * 1024)
const payload = Buffer.from(JSON.stringify({ kind: "note", text: "hi" }))
const message = pool.appendJson(payload, ["note"], Durability.Fast)
console.log(message.toString("utf8"))

pool.close()
client.close()
```

## Remote Client (HTTP/JSON)

```js
const { RemoteClient } = require("plasmite-node")

const client = new RemoteClient("http://127.0.0.1:9700")
const pool = await client.openPool("docs")
const message = await pool.append({ kind: "note", text: "hi" }, ["note"])
console.log(message.seq, message.data)

const tail = await pool.tail({ sinceSeq: message.seq, maxMessages: 1, timeoutMs: 500 })
console.log(await tail.next())
tail.cancel()
```
