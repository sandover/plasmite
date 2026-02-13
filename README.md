# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**Easy interprocess communication.**

Interprocess communication should not be the hard part.

- readers and writers should be able to come and go freely
- messages should be durably stored on disk
- and yet it should be fast
- you shouldn't need a server for local communication
- messages should be inspectable for humans
- schemas should be optional
- you should never fill your disk accidentally

So, there's **Plasmite**.

```bash
pls poke chat --create '{"from": "alice", "msg": "hello world"}'

# In another terminal:
pls peek chat
#  { "data": {"from": "alice", "msg": "hello world"},"meta": {}, ... }
```

Plasmite is a CLI and library suite (Rust, Python, Go, Node, C) for sending and receiving JSON messages through persistent, disk-backed ring buffers called "pools". No daemon, no broker, no config. ~600k msg/sec on a laptop. Crash-safe writes.

For IPC across machines, `pls serve` exposes your local pools securely, and serves a minimal web UI too.

## Why not just...

| | The problem you'll have | Plasmite |
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

# Save JSONL, then replay from file
pls peek incidents --tail 100 --format jsonl --data-only > incidents.jsonl
pls poke incidents-archive -f incidents.jsonl
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

### Browser app on another origin (CORS)

If your web app is not served by `pls serve` itself (for example `https://demo.wratify.ai`), the browser needs explicit CORS permission from the pool server.

```bash
# example: public read-only endpoint for one trusted web origin
pls serve \
  --bind 0.0.0.0:9100 \
  --allow-non-loopback \
  --access read-only \
  --cors-origin https://demo.wratify.ai
```

Use `--cors-origin` multiple times to allow multiple origins.

Important:
- For HTTPS web apps, the pool endpoint must also be HTTPS.
- `--cors-origin` expects exact origins only (`scheme://host[:port]`), no wildcard.
- Avoid embedding long-lived bearer tokens in public frontend JavaScript.

See the [remote protocol spec](spec/remote/v0/SPEC.md) for the full HTTP/JSON API.

## Install

| Channel | Command | CLI | Library/Bindings | Notes |
|---|---|---|---|---|
| Homebrew | `brew install sandover/tap/plasmite` | Yes | Yes (`libplasmite`, header, pkg-config) | Recommended for macOS and Go users. |
| Rust crate (CLI) | `cargo install plasmite` | Yes | No | Installs `plasmite` + `pls`. |
| Rust crate (lib) | `cargo add plasmite` | No | Rust API | Use in Rust apps. |
| Python | `uv tool install plasmite` | Yes | Python bindings | Wheel bundles native assets on supported targets. |
| Python (project dep) | `uv add plasmite` | Optional | Python bindings | Use from existing uv-managed project. |
| Node | `npm i -g plasmite` | Optional | Node bindings | Bundles addon + native assets. |
| Go | `go get github.com/sandover/plasmite/bindings/go/plasmite` | No | Go bindings | Requires system SDK (`brew install ...` first). |
| Release tarball | Download from [releases](https://github.com/sandover/plasmite/releases) | Yes | Yes (SDK layout) | Contains `bin/`, `lib/`, `include/`, `lib/pkgconfig/`. |

### Maintainer Registry Setup (One-Time)

Release automation publishes to crates.io, PyPI, and npm. Configure these repo secrets in GitHub before tagging:

- `CARGO_REGISTRY_TOKEN`: crates.io API token with publish rights for `plasmite`.
- `PYPI_API_TOKEN`: PyPI API token for publishing `plasmite`.
- `NPM_TOKEN`: npm automation token with publish rights for `plasmite`.

Notes:
- PyPI project bootstrap: you do not manually create a project first. The first successful upload creates `plasmite` (if the name is available).
- PyPI CLI: use `twine` (for example `uvx twine upload dist/*`) when testing manual publish.
- npm CLI: `npm publish` publishes either from a package directory or a `.tgz` built by `npm pack`.
- Go has no separate package registry publish step in this repo; users consume the module directly from GitHub.

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
For scripting, use `--json` with `pool create`, `pool list`, `pool delete`, `doctor`, and `serve check`.

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
| Append throughput | ~600k msg/sec (single writer, M3 MacBook) |
| Read | Lock-free, zero-copy via mmap |
| Message overhead (framing) | 72-79 bytes per message (64B header + 8B commit marker + alignment) |
| Default pool size | 1 MB |

**How lookups work**: By default each pool includes an inline index — a fixed-size hash table that maps sequence numbers to byte offsets. When you fetch a message by seq (`get POOL 42`), the index usually provides a direct jump to the right location. If that slot was overwritten by a newer message (hash collision) or is stale, the reader scans forward from the oldest message until it finds the target. You can set `--index-capacity` at pool creation time.

Algorithmic complexity below uses **N** = visible messages in the pool (depends on message sizes and pool capacity), **M** = index slot count.

| Operation | Complexity | Notes |
|---|---|---|
| Append | O(1) + O(payload bytes) | Writes one frame, updates one index slot, publishes the header. `durability=flush` adds OS flush cost. |
| Get by seq (`get POOL SEQ`) | Usually O(1); O(N) worst case | If the index slot matches, it's a direct jump. If the slot is overwritten/stale/invalid (or M=0), it scans forward from the tail until it finds (or passes) the target seq. |
| Tail / peek (`peek`, `export --tail`) | O(k) to emit k; then O(1)/message | Steady-state work is per message. Tag filters are cheap; `--where` runs a jq predicate per message. |
| Export range (`export --from/--to`) | O(R) | Linear in the number of exported messages. |
| Validate (`doctor`, `pool info` warnings) | O(N) | Full ring scan. Index checks are sampled/best-effort diagnostics. |

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
const { Client, Durability } = require("plasmite")
const client = new Client("./data")
const pool = client.createPool("events", 1024 * 1024)
pool.appendJson(Buffer.from('{"sensor": "temp", "value": 23.5}'), [], Durability.Fast)
```

See [Go quickstart](docs/record/go-quickstart.md), [Python docs](bindings/python/README.md), and [Node docs](bindings/node/README.md).

## More

**Specs**: [CLI](spec/v0/SPEC.md) | [API](spec/api/v0/SPEC.md) | [Remote protocol](spec/remote/v0/SPEC.md)

**Guides**: [Rust API quickstart](docs/record/api-quickstart.md) | [Go quickstart](docs/record/go-quickstart.md) | [libplasmite C ABI](docs/record/libplasmite.md) | [Distribution](docs/record/distribution.md) | [Exit codes](docs/record/exit-codes.md) | [Diagnostics](docs/record/doctor.md)

**Contributing**: `docs/contributing.md`

[Changelog](CHANGELOG.md) | Inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma).

## License

MIT. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for vendored code.
