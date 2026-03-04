# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![Homebrew](https://img.shields.io/homebrew/v/sandover/tap/plasmite?logo=homebrew)](https://github.com/sandover/homebrew-tap)
[![crates.io](https://img.shields.io/crates/v/plasmite?logo=rust)](https://lib.rs/crates/plasmite)
[![PyPI](https://img.shields.io/pypi/v/plasmite?logo=pypi)](https://pypi.org/project/plasmite/)
[![npm](https://img.shields.io/npm/v/plasmite?logo=npm)](https://registry.npmjs.org/plasmite)
[![Go Reference](https://pkg.go.dev/badge/github.com/sandover/plasmite/bindings/go/local.svg)](https://pkg.go.dev/github.com/sandover/plasmite/bindings/go/local)
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

Plasmite is a CLI and library suite (Rust, Python, Go, Node, C) for sending and receiving JSON messages through persistent, disk-backed channels called "pools", which are ring buffers. There's no daemon or broker for local IPC, no fancy config, and it's fast (~60k 1KB msgs/sec writes, ~3M msgs/sec reads on a laptop). Readers mmap the pool file and walk frames in place, and payloads use [Lite3](https://github.com/fastserial/lite3), a zero-copy JSON binary encoding.

For IPC across machines, `pls serve` exposes local pools securely, runs an MCP server, and serves a minimal web UI too.

#### Local IPC

<table width="100%">
  <tr>
    <th align="left">Alice</th>
    <th align="left">Bob (a local reader)</th>
  </tr>
  <tr>
    <td valign="top">
      <b>Alice creates a channel (aka a pool)</b><br/>
      <code>pls pool create channel</code>
      <br/><br/>
      <b>Alice sends a message</b><br/>
      <code>pls feed channel</code><br/>
      <code>'{"from": "A", "msg": "hello world"}'</code>
    </td>
    <td valign="bottom">
    <br/>
      <br/><b>Bob starts watching</b><br/>
      <code>pls follow channel</code>
      <br/><br/><br/>
      <b>Bob sees it on stdout</b><br/>
      <code>{ "data": {"from": "A", "msg": "hello world"}, ... }</code>
    </td>
  </tr>
</table>

#### Remote IPC

<table width="100%">
  <tr>
    <th align="left">Alice</th>
    <th align="left">Bob</th>
    <th align="left">Carol (remote)</th>
  </tr>
  <tr>
    <td valign="top">
      <b>Alice runs pool server</b><br/>
      <code>pls serve init</code><br/>
      <code>pls serve</code>
      <br/><br/><br/>
      <b>Alice sends</b><br/>
      <code>pls feed channel</code><br/>
      <code>'{"from": "A", "msg": "hi all"}'</code>
    </td>
    <td valign="bottom">
      <br/><br/>
      <i>(Bob never quit his follow process, so he's <u>still watching the same pool</u>.)</i>
      <br/><br/>
      <br/><br/><br/>
      <b>Bob sees it</b><br/>
      <code>{ "data": {"from": "A", "msg": "hi all"}, ... }</code>
    </td>
    <td valign="bottom">
      <b>Carol watches remotely</b><br/>
      <code>pls follow 
  http://alice:9700/channel</code>
      <br/><br/><br/><br/>
      <b>Carol sees it</b><br/>
      <code>{ "data": {"from": "A", "msg": "hi all"}, ... }</code>
    </td>
  </tr>
</table>

The APIs work the same way as the CLI.

## Comparison with other styles of IPC

| | Drawbacks | Plasmite |
|---|---|---|
| **Kafka**, **RabbitMQ**,  | Lots of machinery: partitions, groups, exchanges, bindings, oh my. | Covers the 80/20 cases: no config, no broker, no partitions, no topology. |
| **Redis / NATS** | Server required even for local messaging. Messages live in server memory; if the server dies, messaging stops. | Pools persist on disk independent of any process. Server only if you need one. |
| **Log files / `tail -f`** | Messages are unstructured. Logs grow and must be rotated (which breaks `tail -f`). Can't easily replay from a specific point. No remote access without setting up syslog. | Messages have structure and sequence numbers. Disk usage is bounded. Replay from any point. Remote access is idiomatic. |
| **Ad-hoc files (temp files, locks, polled dirs)** | Readers have to poll for new files. Locking is manual; crashes leave a stale lock. Files accumulate. No ordering unless you bake it into filenames. | Readers stream in real time. Writers append concurrently without explicit locks, and messages are ordered. Ring buffer bounds disk usage.  |
| **SQLite as a queue** | Readers have to poll. Writers contend. Have to design & migrate schemas. SQLite explicitly discourages network access to the DB file. | Follow & replay without polling. No `SQLITE_BUSY`. No schema, no migrations, no cleanup, easy remote access. |
| **OS primitives (pipes, sockets, shm)** | Named pipes mean if the reader dies, the writer blocks or gets SIGPIPE. With sockets you have to implement your own framing and reconnection. Shared memory has to be coordinated with semaphores; be careful not to crash while holding a lock. Machine-local only. | Many readers, many writers, crash-safe, persistent across reboots. |
| **ZeroMQ** | Messages vanish when processes restart. The pattern matrix is expressive but hard to get right. Binary protocol means you can't inspect messages with standard tools. | Messages persist. One mental model fits most cases. Plain JSON you can pipe through `jq`. |

**Use cases** — CI gates, live event streams, duplex chat, system log ring buffers, replay & debug: see the **[Cookbook](docs/cookbook.md)**. 

Plasmite is for single-host and host-adjacent messaging. If you need multi-host cluster replication, schema registries, or workflow orchestration, see [When Plasmite Isn't the Right Fit](docs/cookbook.md#when-plasmite-isnt-the-right-fit).

## Install

### macOS

```bash
brew install sandover/tap/plasmite
```

Installs the CLI (`plasmite` + `pls`) and the full SDK (`libplasmite`, C header, pkg-config). Go bindings link against this SDK, so install Homebrew first if using Go.

### Rust

```bash
cargo install plasmite     # CLI only
cargo add plasmite         # use as a library in Rust projects
```

### Python

```bash
uv tool install plasmite   # standalone CLI + Python bindings
uv add plasmite            # add to a uv-managed project
```

The wheel includes pre-built native bindings.

### Node

```bash
npm i -g plasmite
```

Package includes pre-built native bindings.

### Go

```bash
go get github.com/sandover/plasmite/bindings/go/local
```

Bindings only (no CLI). Links against `libplasmite` via cgo, so first get the SDK via Homebrew on macOS, or from a [GitHub Releases](https://github.com/sandover/plasmite/releases) tarball on Linux.

### Pre-built binaries

Tarballs for Linux and macOS are on [GitHub Releases](https://github.com/sandover/plasmite/releases). Each archive contains `bin/`, `lib/`, `include/`, and `lib/pkgconfig/`.

Windows builds (`x86_64-pc-windows-msvc`) are available via npm and PyPI. See the [distribution docs](docs/record/distribution.md) for the full install matrix.

## Command Overview

**Messaging**

| | |
|---|---|
| `feed` *pool* *data* | Send a message |
| `follow` *pool* | Follow messages |
| `fetch` *pool* *seq* | Fetch one message by sequence number |
| `duplex` *pool* | 2-way session with a pool |

**Pool management**

| | |
|---|---|
| `pool create` *name* | Create a pool |
| `pool list` | List pools |
| `pool info` *name* | Show pool metadata and metrics |
| `pool delete` *name…* | Delete one or more pools |
| `doctor` *pool* ǀ `--all` | Validate pool integrity |

**Server**

| | |
|---|---|
| `serve` | HTTP server |

`pls` and `plasmite` are the same binary. Shell completion: `plasmite completion bash|zsh|fish`.

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
| Write throughput | ~60k msg/sec (1KB payload, single writer) |
| Read throughput | ~3M msg/sec (in-process, lock-free) |
| Indexed lookup | ~1.5M lookups/sec |
| Message overhead | 72–79 bytes (header + commit marker + alignment) |
| Default pool size | 1 MB |

Measured locally on M3 MacBook, `Durability::Fast`. Reproduce with `scripts/bench_runtime_lanes.sh`.

**How reads work**: The pool file is memory-mapped. Readers walk frames directly — no read syscalls, no buffer copies. Payloads use [Lite3](https://github.com/fastserial/lite3), a zero-copy binary encoding that supports field lookup by offset, so tag filtering and `--where` predicates run without deserializing the full message. JSON conversion happens only at the output boundary.

**How writes work**: Writers acquire an OS file lock, write the frame as `Writing`, flip it to `Committed`, and update the header. The lock is held only for memcpy + header update — no allocation or encoding under the lock.

**How lookups work**: Each pool has an inline index (hash table mapping seq → byte offset). `fetch POOL 42` jumps directly to the frame. If the slot is stale or collided, it scans forward from the tail.

| Operation | Complexity |
|---|---|
| Append | O(1) |
| Fetch by seq | O(1) typical, O(N) worst case |
| Follow / tail | O(1) per message |
| Replay window | O(R) messages replayed |

## More

**Specs**: [CLI](spec/v0/SPEC.md) | [API](spec/api/v0/SPEC.md) | [Remote protocol](spec/remote/v0/SPEC.md)

**Bindings**: [Go](bindings/go/README.md) | [Python](bindings/python/README.md) | [Node](bindings/node/README.md)

**Guides**: [Serving & remote access](docs/record/serving.md) | [Distribution](docs/record/distribution.md)

**Contributing**: See `AGENTS.md` for CI hygiene; `docs/record/releasing.md` for release process


[Changelog](CHANGELOG.md) | Inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma).

## License

MIT. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for vendored code.
