# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**Easy interprocess communication.**

Interprocess communication should not be hard!

- messages should be durable -- they live on disk
- readers and writers should come and go freely
- you shouldn't need a server for local IPC
- messages should be easy for humans to read
- schemas should be optional
- it should be fast
- you should never fill your disk accidentally

So, there's **Plasmite**.

```bash
pls poke chat --create '{"from": "alice", "msg": "hello world"}'

# In another terminal:
pls peek chat
#   {"seq":1,"time":"...","meta":{"tags":[]},"data":{"from":"alice","msg":"hello world"}}
```

Plasmite is a CLI and library suite (Rust, Python, Go, Node, C) for sending and receiving JSON messages through persistent, disk-backed ring buffers called "pools". No daemon, no broker, no config. ~600k msg/sec on a laptop. Crash-safe writes. Pools are bounded, so you can write forever without filling your disk.

For IPC across machines, `pls serve` exposes your local pools, with TLS support and a little web UI too.

## Install

```bash
brew install sandover/tap/plasmite    # macOS
cargo install plasmite                # anywhere with Rust
```

Installs both `plasmite` and the `pls` shorthand.

Prefer manual binaries? Grab your platform tarball from the
[v0.1.0 release](https://github.com/sandover/plasmite/releases/tag/v0.1.0).

## Why not just...

| | The problem | Plasmite |
|---|---|---|
| **Log files / `tail -f`** | Unstructured, grow forever, no sequence numbers, fragile parsing | Plasmite messages are structured JSON with sequence numbers, and disk usage stays bounded, so you can filter with tags or jq and never worry about runaway logs. |
| **Temp files + locks** | No streaming, easy to corrupt, readers block writers | Plasmite lets many writers append concurrently, and readers stream in real time without blocking, so you never have to worry about corruption or contention. |
| **Redis / NATS** | Another server to run and monitor | Plasmite pools are just files on disk — no daemon, no ports, no config — so there's nothing to keep running. |
| **SQLite as a queue** | Polling-based, write contention, schema and cleanup are on you | Plasmite readers stream without polling and writers don't contend. There's no schema or maintenance to think about. |
| **Named pipes** | One reader at a time, writers block, nothing persists | Plasmite supports any number of reading and writing processes, and the messages persist on disk, so they survive restarts. |
| **Unix domain sockets** | Stream-oriented, no message framing, no persistence, one-to-one | Plasmite messages have boundaries and sequence numbers built in, and any number of readers can watch the same pool, so fan-out is free. |
| **Poll a directory** | Busy loops, no ordering, files accumulate forever | Plasmite streams messages to readers in sequence order, and the ring buffer has a known disk footprint, no files accumulating. |
| **Shared memory** | No persistence, painful to coordinate, binary formats | Plasmite messages are durable JSON on disk, and readers are lock-free, so you get persistence and coordination without the pain. |
| **ZeroMQ** | No persistence, complex pattern zoo, binary protocol, library in every process | Plasmite messages are durable and human-readable by default, and you can get started with one CLI command or library call, so there's no pattern vocabulary to learn. |

## What it looks like

### Two terminals, two processes, one pool

**Terminal 1** — your build script might write progress:
```bash
pls poke build --create '{"step": "compile", "status": "running"}'
sleep 2
pls poke build '{"step": "compile", "status": "done"}'
pls poke build '{"step": "test", "status": "running"}'
```

**Terminal 2** — you, watching:
```bash
pls peek build
```

### Gate one script on another

```bash
# deploy.sh — block until tests go green
pls peek ci --where '.data.status == "green"' --one > /dev/null
echo "Tests passed, deploying..."

# test-runner.sh — signal when done
pls poke ci --create '{"status": "green", "commit": "abc123"}'
```

Three lines of coordination. No polling, no lock files, no shared database.

### Funnel anything into a pool

```bash
# Ingest an API event stream
curl -N https://api.example.com/events | pls poke events --create

# Tee system logs
journalctl -o json-seq -f | pls poke syslog --create    # Linux
/usr/bin/log stream --style ndjson | pls poke syslog --create  # macOS
```

### Filter, tag, replay

```bash
# Tag on write
pls poke incidents --tag sev1 --tag billing '{"msg": "payment gateway timeout"}'

# Filter on read
pls peek incidents --tag sev1
pls peek incidents --where '.data.msg | test("timeout")'

# Replay the last hour at 10x
pls peek incidents --since 1h --replay 10
```

### Go remote

```bash
pls serve                          # local-only by default
pls serve init                     # bootstrap TLS + token for LAN access
```

Same CLI, just pass a URL:

```bash
pls poke http://server:9700/events '{"sensor": "temp", "value": 23.5}'
pls peek http://server:9700/events --tail 20
```

A built-in web UI lives at `/ui`:

![Plasmite UI pool watch](docs/images/ui/ui-pool-watch.png)

See the [remote protocol spec](spec/remote/v0/SPEC.md) for the full HTTP/JSON API.

## Commands

| Command | What it does |
|---|---|
| `poke POOL DATA` | Send a message (`--create` to auto-create the pool) |
| `peek POOL` | Watch messages (`--create` auto-creates missing local pools) |
| `get POOL SEQ` | Fetch one message by sequence number |
| `pool create NAME` | Create a pool (`--size 8M` for larger) |
| `pool list` | List pools |
| `pool info NAME` | Show pool metadata and metrics |
| `pool delete NAME...` | Delete one or more pools |
| `doctor POOL \| --all` | Validate pool integrity |
| `serve` | HTTP server (loopback default; non-loopback opt-in) |

`pls` and `plasmite` are the same binary. Shell completion: `plasmite completion bash|zsh|fish`.
Remote pools support read and write; `--create` is local-only.

## How it works

A pool is a single `.plasmite` file containing a persistent ring buffer:

- **Multiple writers** append concurrently (serialized via OS file locks)
- **Multiple readers** watch concurrently (lock-free, zero-copy)
- **Bounded retention** — old messages overwritten when full (default 1 MB, configurable)
- **Crash-safe** — torn writes never propagate

Every message carries a **seq** (monotonic), a **time** (nanosecond precision), optional **tags**, and your JSON **data**. Tags and `--where` (jq predicates) compose for filtering. See the [pattern matching guide](docs/record/pattern-matching.md).

Default pool directory: `~/.plasmite/pools/`.

## Performance

| Metric | |
|---|---|
| Append throughput | ~600k msg/sec (single writer, M1 MacBook) |
| Read | Lock-free, zero-copy via mmap |
| Message overhead (framing) | 72-79 bytes per message (64B header + 8B commit marker + alignment) |
| Default pool size | 1 MB (~20k messages) |
| Max tested pool | 1 GB (local create+poke+get smoke test) |

## Bindings

Native bindings — no subprocess overhead, no serialization tax:

```go
client, _ := plasmite.NewClient("./data")
pool, _ := client.CreatePool(plasmite.PoolRefName("events"), 1024*1024)
pool.Append(map[string]any{"sensor": "temp", "value": 23.5}, nil, plasmite.DurabilityFast)
```

```python
from plasmite import Client, Durability
client = Client("./data")
pool = client.create_pool("events", 1024*1024)
pool.append_json(b'{"sensor": "temp", "value": 23.5}', [], Durability.FAST)
```

```javascript
const { Client, Durability } = require("plasmite-node")
const client = new Client("./data")
const pool = client.createPool("events", 1024 * 1024)
pool.appendJson(Buffer.from('{"sensor": "temp", "value": 23.5}'), [], Durability.Fast)
```

```bash
pip install plasmite              # or: uv pip install plasmite
npm install plasmite-node
go get github.com/sandover/plasmite/bindings/go/plasmite
```

Python and Node bindings are source-only for v0.1.0 (Rust toolchain required — `brew install rust` on macOS, [rustup](https://rustup.rs) on Linux). Pre-built binaries coming soon.

See [Go quickstart](docs/record/go-quickstart.md), [Python docs](bindings/python/README.md), and [Node docs](bindings/node/README.md).

## Runtime parsing

Plasmite v0 uses a **simd-json-only** runtime parser path (no optional fallback parser feature toggles).

- Parsing behavior contract: [`docs/decisions/simd-json-parser-contract.md`](docs/decisions/simd-json-parser-contract.md)
- Portability/support assumptions: [`docs/record/simd-json-rollout.md`](docs/record/simd-json-rollout.md)
- Parse failures surface stable category labels in hints/notices (for example `syntax`,
  `utf8`, `numeric-range`, `depth-limit`, `unknown`) plus context identifiers.

## More

**Specs**: [CLI](spec/v0/SPEC.md) | [API](spec/api/v0/SPEC.md) | [Remote protocol](spec/remote/v0/SPEC.md)

**Guides**: [Rust API quickstart](docs/record/api-quickstart.md) | [Go quickstart](docs/record/go-quickstart.md) | [libplasmite C ABI](docs/record/libplasmite.md) | [Distribution](docs/record/distribution.md) | [Exit codes](docs/record/exit-codes.md) | [Diagnostics](docs/record/doctor.md)

[Changelog](CHANGELOG.md) | Inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma).

## License

MIT. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for vendored code.
