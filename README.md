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

| | Drawbacks | Plasmite |
|---|---|---|
| **Log files / `tail -f`** | Unstructured, grow forever, no sequence numbers, fragile parsing | Structured JSON with sequence numbers and bounded disk usage. Filter with tags or jq; never worry about runaway logs. |
| **Temp files + locks** | No streaming, easy to corrupt, readers block writers | Many writers append concurrently, readers stream in real time without blocking. No corruption or contention. |
| **Redis / NATS** | Another server to run and monitor; overkill for single-host messaging | Just files on disk — no daemon, no ports, no config. If you only need local or host-adjacent messaging, don't introduce a broker. |
| **SQLite as a queue** | Polling-based, write contention, schema and vacuuming are on you | Purpose-built message stream: follow/replay semantics, concurrent writers, no schema, no cleanup logic, no polling. |
| **Named pipes** | One reader at a time, writers block, nothing persists | Any number of reading and writing processes; messages persist on disk and survive restarts. |
| **Unix domain sockets** | Stream-oriented, no message framing, no persistence, one-to-one | Message boundaries and sequence numbers built in. Any number of readers can watch the same pool — fan-out is free. |
| **Poll a directory** | Busy loops, no ordering, files accumulate forever | Messages stream to readers in sequence order; the ring buffer has a known disk footprint. |
| **Shared memory** | No persistence, painful to coordinate, binary formats | Durable JSON on disk with lock-free readers. Persistence and coordination without the pain. |
| **ZeroMQ** | No persistence, complex pattern zoo, binary protocol, library in every process | Durable and human-readable by default. One CLI command or library call to get started; no pattern vocabulary to learn. |
| **Language-specific queue libs** | Tied to one runtime; no CLI, no cross-language story | Consistent CLI + multi-language bindings (Rust, Python, Go, Node, C) + versioned on-disk format. An ecosystem surface, not a single-language helper. |

## Real world use cases

### Build event bus

Multiple build steps write progress into one pool; you watch from another terminal.

```bash
pls poke build --create '{"step": "compile", "status": "done"}'
pls poke build '{"step": "test", "status": "running"}'

# elsewhere:
pls peek build
```

### CI gate

A deploy script blocks until the test runner signals "green" — no polling, no lock files.

```bash
# deploy.sh
pls peek ci --where '.data.status == "green"' --one > /dev/null && ./deploy.sh

# test-runner.sh
pls poke ci --create '{"status": "green", "commit": "abc123"}'
```

### System log intake

Pipe structured logs into a bounded pool so they never fill your disk, then replay for debugging.

```bash
journalctl -o json-seq -f | pls poke syslog --create       # Linux
pls peek syslog --since 30m --replay 1                       # replay last 30 min
```

### Tagged incident stream

Tag events on write, filter on read, replay at speed.

```bash
pls poke incidents --create --tag sev1 '{"msg": "payment gateway timeout"}'
pls peek incidents --tag sev1 --where '.data.msg | test("timeout")'
pls peek incidents --since 1h --replay 10
```

### Remote pools

Expose local pools over HTTP; clients use the same CLI with a URL.

```bash
pls serve                          # loopback-only by default
pls serve init                     # bootstrap TLS + token for LAN access

pls poke http://server:9700/events '{"sensor": "temp", "value": 23.5}'
pls peek http://server:9700/events --tail 20
```

A built-in web UI lives at `/ui`:

![Plasmite UI pool watch](docs/images/ui/ui-pool-watch.png)

For CORS, auth, and deployment details, see [Serving & remote access](docs/record/serving.md) and the [remote protocol spec](spec/remote/v0/SPEC.md).

More examples — polyglot producer/consumer, multi-writer event bus, API stream ingest, CORS setup — in the **[Cookbook](docs/cookbook.md)**.

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
| Windows preview zip | Download `plasmite_<version>_windows_amd64_preview.zip` from [releases](https://github.com/sandover/plasmite/releases) | Yes | Partial SDK (`bin/`, `lib/`, `include/`) | Best-effort preview for `x86_64-pc-windows-msvc`; not yet an officially supported release channel. |

### Windows preview support (best-effort)

- Scope: `x86_64-pc-windows-msvc` preview binaries attached to GitHub releases as `*_windows_amd64_preview.zip` + `.sha256`.
- Support level: best-effort preview. Windows is not yet promoted to fully supported release-gating status.
- Recommended use today: prefer remote pool workflows (Windows CLI client + `plasmite serve` on Linux/macOS host) for higher reliability.

Remote-only fallback pattern:

```bash
# Windows client example
plasmite peek http://<host>:9700/events --tail 20 --format jsonl
plasmite poke http://<host>:9700/events '{"kind":"win-preview","ok":true}'
```

Troubleshooting (Windows preview):

- Source build fails with `cl.exe` errors like `__builtin_expect` / `__attribute__`:
  - Use preview release assets instead of local source compilation.
- Local write fails with `failed to encode json as lite3`:
  - Use remote refs (`http://host:port/<pool>`) so encoding happens server-side.
- Download integrity check:
  - Run `certutil -hashfile plasmite_<version>_windows_amd64_preview.zip SHA256` and compare with the shipped `.sha256` file.

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

Every message carries a **seq** (monotonic), a **time** (nanosecond precision), optional **tags**, and your JSON **data**. Tags and `--where` (jq predicates) compose for filtering. See the [CLI spec § pattern matching](spec/v0/SPEC.md).

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

See [Go bindings](bindings/go/README.md), [Python bindings](bindings/python/README.md), and [Node bindings](bindings/node/README.md).

## More

**Specs**: [CLI](spec/v0/SPEC.md) | [API](spec/api/v0/SPEC.md) | [Remote protocol](spec/remote/v0/SPEC.md)

**Guides**: [Serving & remote access](docs/record/serving.md) | [Distribution](docs/record/distribution.md)

**Contributing**: See `AGENTS.md` for CI hygiene; `docs/record/releasing.md` for release process


[Changelog](CHANGELOG.md) | Inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma).

## License

MIT. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for vendored code.
