# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**Easy interprocess communication.**

What would it take to make IPC pleasant and predictable?

- Reading and writing processes come and go... so **message channels should outlast them**
- Machines crash... so **channels should persist on disk**
- Disks are finite... so **channels should be bounded in size**
- Message brokers bring complexity and ceremony... so for local IPC, **don't require a broker**
- Observability matters... so **messages must be inspectable**
- Schemas are great... but **schemas should be optional**
- Latency matters... so **IPC should be fast**, zero-copy wherever possible

So, there's **Plasmite**.

Plasmite is a CLI and library suite (Rust, Python, Go, Node, C) for sending and receiving JSON messages through persistent, disk-backed channels called "pools", which are ring buffers. There's no daemon or broker for local IPC, no fancy config, and it's quick (~600k msg/sec on a laptop). Readers mmap the pool file and walk frames in place, and payloads use [Lite3](https://github.com/fastserial/lite3), a zero-copy JSON binary encoding.

For IPC across machines, `pls serve` exposes local pools securely, runs an MCP server, and serves a minimal web UI too.

#### Local IPC

<table width="100%">
  <tr>
    <th align="left">Alice's terminal</th>
    <th align="left">Bob's terminal</th>
  </tr>
  <tr>
    <td valign="top">
      <b>Alice creates a channel</b><br/>
      <code>pls pool create channel</code>
      <br/><br/>
      <br/>
      <b>Alice writes a message</b><br/>
      <code>pls feed channel</code><br/>
      <code>'{"from": "alice", "msg": "hello world"}'</code>
    </td>
    <td valign="bottom">
    <br/>
      <br/><b>Bob starts reading</b><br/>
      <code>pls follow channel</code>
      <br/><br/>
      <br/>
      <br/>
      <br/>
      <b>Bob sees it on stdout</b><br/>
      <code>{ "data": {"from": "alice", "msg": "hello world"}, ... }</code>
    </td>
  </tr>
</table>

#### Remote IPC

<table width="100%">
  <tr>
    <th align="left">Alice</th>
    <th align="left">Bob</th>
    <th align="left">Carol</th>
  </tr>
  <tr>
    <td valign="top">
      <b>Alice runs pool server</b><br/>
      <code>pls serve init</code><br/>
      <code>pls serve</code>
      <br/><br/><br/>
      <b>Alice writes</b><br/>
      <code>pls feed channel</code><br/>
      <code>'{"from": "alice", "msg": "hi all"}'</code>
    </td>
    <td valign="bottom">
      <br/><br/>
      <i>(Bob never quit his follow process, so he's still watching.)</i>
      <br/><br/>
      <br/><br/><br/>
      <b>Bob sees it</b><br/>
      <code>{ "data": {"from": "alice", "msg": "hi all"}, ... }</code>
    </td>
    <td valign="bottom">
      <b>Carol follows remotely</b><br/>
      <code>pls follow http://alice:9700/channel</code>
      <br/><br/><br/><br/><br/>
      <b>Carol sees it</b><br/>
      <code>{ "data": {"from": "alice", "msg": "hi all"}, ... }</code>
    </td>
  </tr>
</table>

The APIs work the same way as the CLI.

## Why not just...

| | Drawbacks | Plasmite |
|---|---|---|
| **Kafka**, **RabbitMQ**,  | Lots of machinery: partitions, groups, exchanges, bindings, oh my. | No config, no broker, no partitions, no topology — `feed` and `follow`. |
| **Redis / NATS** | Server required even for local messaging. Messages live in server memory; if the server dies, messaging stops. | Pools persist on disk independent of any process. No server needed locally. |
| **Log files / `tail -f`** | Messages are unstructured. Logs grow and have to be rotated (and rotating logs breaks `tail -f`). No way to replay from a specific point. No remote access without setting up syslog. | Messages are JSON with sequence numbers. Bounded disk usage. Replay from any point. Remote access built in. |
| **Ad-hoc files (temp files, locks, polled dirs)** | Readers have to poll for new files. Locking is manual; crashes leave a stale lock. Files accumulate. No ordering unless you bake it into filenames. | Readers stream in real time. Writers append concurrently without explicit locks, and messages are ordered. Ring buffer bounds disk usage.  |
| **SQLite as a queue** | Readers have to poll. Writers contend. Have to design & migrate schemas. SQLite explicitly discourages network access to the DB file. | Follow & replay without polling. No `SQLITE_BUSY`. No schema, no migrations, no cleanup, easy remote access. |
| **OS primitives (pipes, sockets, shm)** | Named pipes mean if the reader dies, the writer blocks or gets SIGPIPE. With sockets you have to implement your own framing and reconnection. Shared memory has to be coordinated with semaphores, and a crash while holding a lock is bad news. Machine-local only. | Many readers, many writers, crash-safe, persistent across reboots. Works over the network too. |
| **ZeroMQ** | Messages vanish when processes restart. The pattern matrix is expressive but hard to get right. Binary protocol means you can't inspect messages with standard tools. | Messages persist across restarts. One model: append, follow, replay. Plain JSON you can pipe through `jq`. |

**Use cases** — CI gates, live event streams, duplex chat, system log ring buffers, replay & debug — all in the **[Cookbook](docs/cookbook.md)**. 

Plasmite is for single-host and host-adjacent messaging. If you need multi-host cluster replication, schema registries, or workflow orchestration, see [When Plasmite Isn't the Right Fit](docs/cookbook.md#when-plasmite-isnt-the-right-fit).

## Install

### macOS

```bash
brew install sandover/tap/plasmite
```

Installs the CLI (`plasmite` + `pls`) and the full SDK (`libplasmite`, C header, pkg-config). Go bindings link against this SDK, so install Homebrew first if you're using Go.

### Rust

```bash
cargo install plasmite     # CLI only (plasmite + pls)
cargo add plasmite         # use as a library in your Rust project
```

### Python

```bash
uv tool install plasmite   # standalone CLI + Python bindings
uv add plasmite            # add to an existing uv-managed project
```

The wheel includes pre-built native bindings.

### Node

```bash
npm i -g plasmite
```

The package includes pre-built native bindings.

### Go

```bash
go get github.com/sandover/plasmite/bindings/go/local
```

Bindings only (no CLI). Links against `libplasmite` via cgo, so you'll need the SDK on your system first — via Homebrew on macOS, or from a [GitHub Releases](https://github.com/sandover/plasmite/releases) tarball on Linux.

### Pre-built binaries

Tarballs for Linux and macOS are on [GitHub Releases](https://github.com/sandover/plasmite/releases). Each archive contains `bin/`, `lib/`, `include/`, and `lib/pkgconfig/`.

Windows builds (`x86_64-pc-windows-msvc`) are available via npm and PyPI. See the [distribution docs](docs/record/distribution.md) for the full install matrix.

## Commands

| Command | What it does |
|---|---|
| `feed POOL DATA` | Send a message (`--create` to auto-create the pool) |
| `follow POOL` | Follow messages (`--create` auto-creates missing local pools) |
| `fetch POOL SEQ` | Fetch one message by sequence number |
| `pool create NAME` | Create a pool (`--size 8M` for larger) |
| `pool list` | List pools |
| `pool info NAME` | Show pool metadata and metrics |
| `pool delete NAME...` | Delete one or more pools |
| `duplex POOL` | Read and write from one command (`--me` for chat mode) |
| `doctor POOL \| --all` | Validate pool integrity |
| `serve` | HTTP server (loopback default; non-loopback opt-in) |

`pls` and `plasmite` are the same binary. Shell completion: `plasmite completion bash|zsh|fish`.
Remote pools support read and write; `--create` is local-only.
For scripting, use `--json` with `pool create`, `pool list`, `pool delete`, `doctor`, and `serve check`.

## How it works

A pool is a single `.plasmite` file containing a persistent ring buffer:

- **Multiple writers** append concurrently (serialized via OS file locks)
- **Multiple readers** follow concurrently (lock-free, zero-copy)
- **Bounded retention** — old messages overwritten when full (default 1 MB, configurable)
- **Crash-safe** — processes crash and restart; torn writes never propagate

Every message carries a **seq** (monotonic), a **time** (nanosecond precision), optional **tags**, and your JSON **data**. Tags and `--where` (jq predicates) compose for filtering. See the [CLI spec § pattern matching](spec/v0/SPEC.md).

Default pool directory: `~/.plasmite/pools/`.

## Performance

| Metric | |
|---|---|
| Append throughput | ~600k msg/sec (single writer, M3 MacBook) |
| Read | Lock-free, zero-copy via mmap |
| On-disk format | [Lite3](https://github.com/fastserial/lite3) (zero-copy, JSON-compatible binary); field access without deserialization |
| Message overhead (framing) | 72-79 bytes per message (64B header + 8B commit marker + alignment) |
| Default pool size | 1 MB |

**How reads work**: The pool file is memory-mapped. Readers walk committed frames directly from the mapped region — no read syscalls, no buffer copies. Payloads are stored in [Lite3](https://github.com/fastserial/lite3), a zero-copy binary format that is byte-for-byte JSON-compatible — every valid JSON document has an equivalent Lite3 representation and vice versa. Lite3 supports field lookup by offset, so tag filtering and `--where` predicates run without deserializing the full message. JSON conversion happens only at the output boundary.

**How writes work**: Writers acquire an OS file lock, plan frame placement (including ring wrap), write the frame as `Writing`, then flip it to `Committed` and update the header. The lock is held only for the memcpy + header update — no allocation or encoding happens under the lock.

**How lookups work**: Each pool includes an inline index — a fixed-size hash table mapping sequence numbers to byte offsets. `fetch POOL 42` usually jumps directly to the right frame. If the slot is stale or collided, the reader scans forward from the tail. You can tune this with `--index-capacity` at pool creation time.

Algorithmic complexity below uses **N** = visible messages in the pool (depends on message sizes and pool capacity), **M** = index slot count.

| Operation | Complexity | Notes |
|---|---|---|
| Append | O(1) + O(payload bytes) | Writes one frame, updates one index slot, publishes the header. `durability=flush` adds OS flush cost. |
| Get by seq (`fetch POOL SEQ`) | Usually O(1); O(N) worst case | If the index slot matches, it's a direct jump. If the slot is overwritten/stale/invalid (or M=0), it scans forward from the tail until it finds (or passes) the target seq. |
| Tail / follow (`follow --tail`) | O(k) to emit k; then O(1)/message | Steady-state work is per message. Tag filters are cheap; `--where` runs a jq predicate per message. |
| Replay window (`follow --since ... --replay`) | O(R) | Linear in the number of replayed messages. |
| Validate (`doctor`, `pool info` warnings) | O(N) | Full ring scan. Index checks are sampled/best-effort diagnostics. |

## Bindings

Native bindings:

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
