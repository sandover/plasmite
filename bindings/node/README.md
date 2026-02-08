<!--
Purpose: Document how to build and use the Plasmite Node bindings.
Exports: N/A (documentation).
Role: Quickstart for Node/TypeScript users of libplasmite.
Invariants: Uses the C ABI via N-API and matches v0 semantics.
Notes: Runtime prefers bundled native assets shipped in the npm package.
-->

# Plasmite Node Bindings (v0)

These bindings wrap the `libplasmite` C ABI via a N-API addon.

## Installation

Install from npm (canonical package name):

```bash
npm install plasmite
```

Published package artifacts bundle:
- `index.node` N-API addon
- `libplasmite.(dylib|so)` beside the addon
- `plasmite` CLI binary exposed as `npx plasmite`

For development/testing from this repo, see Build & Test below.

## Build Requirements

- Node 20+
- Rust toolchain (for building the addon)
- `libplasmite` built from this repo (`cargo build -p plasmite`)

## Build & Test

From the repo root:

```bash
cargo build -p plasmite
```

Canonical repo-root command:

```bash
just bindings-node-test
```

Equivalent manual command (from `bindings/node`):

```bash
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" npm test
```

Pack/install smoke (ensures bundled assets work without env vars):

```bash
./scripts/node_pack_smoke.sh
```

## Usage

```js
const { Client, Durability } = require("plasmite")

const client = new Client("./data")
const pool = client.createPool("docs", 64 * 1024 * 1024)
const payload = Buffer.from(JSON.stringify({ kind: "note", text: "hi" }))
const message = pool.appendJson(payload, ["note"], Durability.Fast)
console.log(message.toString("utf8"))

const frame = pool.getLite3(BigInt(1))
console.log(frame.payload.length)

pool.close()
client.close()
```

Local binding failures throw `PlasmiteNativeError` with structured metadata fields (`kind`, `path`, `seq`, `offset`) when available.

## Remote Client (HTTP/JSON)

```js
const { RemoteClient } = require("plasmite")

const client = new RemoteClient("http://127.0.0.1:9700")
const pool = await client.openPool("docs")
const message = await pool.append({ kind: "note", text: "hi" }, ["note"])
console.log(message.seq, message.data)

const tail = await pool.tail({
  sinceSeq: message.seq,
  tags: ["note"],
  maxMessages: 1,
  timeoutMs: 500,
})
console.log(await tail.next())
tail.cancel()
```

`tail({ tags: [...] })` performs exact tag matching and composes with other filters via AND semantics.
