# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![Homebrew Tap](https://img.shields.io/badge/Homebrew-Tap-2EA44F?logo=homebrew&logoColor=white)](https://github.com/sandover/homebrew-tap)
[![crates.io](https://img.shields.io/crates/v/plasmite?logo=rust)](https://crates.io/crates/plasmite)
[![PyPI](https://img.shields.io/pypi/v/plasmite?logo=pypi)](https://pypi.org/project/plasmite/)
[![npm](https://img.shields.io/npm/v/plasmite?logo=npm)](https://www.npmjs.com/package/plasmite)
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
- Latency matters... so **IPC should be fast**

So, there's **Plasmite**.

Plasmite is a CLI and library suite (Rust, Python, Go, Node, C) for sending and receiving JSON messages through persistent, disk-backed channels called "pools", which are ring buffers. There's no daemon or broker for local IPC, no fancy config. Readers mmap the pool file and take owned message snapshots, and payloads use [Lite3](https://github.com/fastserial/lite3), a binary JSON encoding.

For IPC across machines, `pls serve` exposes local pools over HTTPS. Native
clients and browsers connect with an access key. A local MCP process works
directly with local pools; a remote MCP harness can authorize in the browser.
Follow [Share your first pool](docs/record/serving.md#share-your-first-pool).

#### Local IPC

<table width="100%">
  <tr>
    <th align="left">Alice</th>
    <th align="left">Bob (a local reader)</th>
  </tr>
  <tr>
    <td valign="top">
      <b>Alice creates a channel (aka a pool)</b><br/>
      <code>pls --dir ./pools pool create channel</code>
      <br/><br/>
      <b>Alice sends a message</b><br/>
      <code>echo '{"from":"A","msg":"hello world"}' | pls --dir ./pools feed channel</code>
    </td>
    <td valign="bottom">
    <br/>
      <br/><b>Bob starts watching</b><br/>
      <code>pls --dir ./pools follow channel --tail 1</code>
      <br/><br/><br/>
      <b>Bob sees it on stdout</b><br/>
      <code>{ "data": {"from": "A", "msg": "hello world"}, ... }</code>
    </td>
  </tr>
</table>

#### Remote IPC

Replace `alice.example.test` with a hostname Carol can reach.

<table width="100%">
  <tr>
    <th align="left">Alice</th>
    <th align="left">Bob</th>
    <th align="left">Carol (remote)</th>
  </tr>
  <tr>
    <td valign="top">
      <b>Alice starts the server</b><br/>
      <code>pls --dir ./pools serve https://alice.example.test:9743</code><br/>
      <br/>In another terminal on the server:<br/>
      <code>pls --dir ./pools access invite Carol</code>
      <br/><br/>
      Alice sends Carol the HTTPS address and access key privately. The key grants full access to <code>./pools</code>, including creating and deleting pools.
      <br/><br/><br/>
      <b>Alice sends</b><br/>
      <code>echo '{"from":"A","msg":"hi all"}' | pls --dir ./pools feed channel</code>
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
      <b>Carol connects and watches</b><br/>
      <code>pls access connect https://alice.example.test:9743</code><br/>
      <code>pls follow https://alice.example.test:9743/channel</code>
      <br/><br/><br/><br/>
      <b>Carol sees it</b><br/>
      <code>{ "data": {"from": "A", "msg": "hi all"}, ... }</code>
    </td>
  </tr>
</table>

Carol enters the key at the hidden prompt. She can also open the HTTPS address
in a browser to sign in and browse the pools. See [browser access](docs/record/serving.md#open-pools-in-a-browser).

#### UDP sending and receiving

On a controlled network, Alice can also enable UDP. Stop the server with Ctrl+C, then restart it:

```bash
pls --dir ./pools serve https://alice.example.test:9743 --udp=0.0.0.0:9702 --udp-follow
```

Carol sends and watches:

```bash
echo '{"from":"C","msg":"hello"}' | pls feed udp://alice.example.test:9702/channel
pls follow udp://alice.example.test:9702/channel --tail 1
```

UDP is best-effort, with no authentication, encryption, or stored-write acknowledgement. Use it on loopback or a separately protected network. See the [UDP guide](docs/cli.md).

#### Pipes and YAML

Ordinary JSON objects go straight into `feed`. Complete Plasmite messages keep their data and tags when piped to another pool, with a new sequence and time:

```bash
pls --dir ./pools follow channel --no-follow | pls --dir ./pools feed copy --create-size 1M
pls --dir ./pools fetch channel 1 --format yaml | pls --dir ./pools feed copy
```

`--create-size` creates a missing pool; it never resizes or recreates an existing pool. `follow --no-follow` reads all retained messages and exits.

YAML input and output use the same values as JSON. `--format yaml` selects YAML output; marked YAML input is detected automatically.

The APIs work the same way as the CLI.

## Comparison with other styles of IPC

| | Drawbacks | Plasmite |
|---|---|---|
| **Kafka**, **RabbitMQ**,  | Lots of machinery: partitions, groups, exchanges, bindings, oh my. | Covers the 80/20 cases: no config, no broker, no partitions, no topology. |
| **Redis / NATS** | Server required even for local messaging. Persistence and delivery depend on configuration. | Pools persist on disk independent of any process. Server only if you need one. |
| **Log files / `tail -f`** | Structure and sequence numbers depend on the log format. Logs grow and need rotation and reader coordination. Can't easily replay from a specific point. Remote access needs extra setup. | Messages have structure and sequence numbers. Disk usage is bounded. Replay retained messages. Remote access is idiomatic. |
| **Ad-hoc files (temp files, locks, polled dirs)** | Readers have to poll for new files. Locking is manual; crashes leave a stale lock. Files accumulate. No ordering unless you bake it into filenames. | Readers stream messages. Writers append concurrently with file locks managed by the library, and messages are ordered. Ring buffer bounds disk usage.  |
| **SQLite as a queue** | Queue semantics, schemas, retention and notification need application code. Sharing the database file over a network is discouraged. | Built-in follow, replay and bounded retention. Optional schemas and remote access through the server. |
| **OS primitives (pipes, sockets, shm)** | Named pipes mean if the reader dies, the writer blocks or gets SIGPIPE. With sockets you have to implement your own framing and reconnection. Shared memory needs coordination and crash recovery. Persistence needs extra work. | Many readers, many writers, committed messages, persistent across reboots. |
| **ZeroMQ** | Messages vanish when processes restart. The pattern matrix is expressive but hard to get right. Binary protocol means you can't inspect messages with standard tools. | Messages persist. One message model for local and remote use. Plain JSON you can pipe through `jq`. |

**Use cases** — CI gates, live event streams, duplex chat, system log ring buffers, replay & debug: see the **[Cookbook](docs/cookbook.md)**.

Plasmite is for single-host and host-adjacent messaging. If you need multi-host cluster replication, schema registries, or workflow orchestration, see [When Plasmite Isn't the Right Fit](docs/cookbook.md#when-plasmite-isnt-the-right-fit).

## Install

### macOS

```bash
brew install sandover/tap/plasmite
```

Installs the CLI (`plasmite` + `pls`) and the full SDK (`libplasmite`, C header, pkg-config). Go bindings link against this SDK, so install Homebrew first if using Go.

### Linux ARM / Raspberry Pi (preview)

The published [1.0.0 release](https://github.com/sandover/plasmite/releases/tag/v1.0.0)
includes ARM archives with `plasmite`, `pls`, and the full SDK. You can run
local pools and the HTTPS server without installing Rust or a desktop.

| Linux userland | Archive suffix | Minimum |
| --- | --- | --- |
| ARM64 (64-bit) | `linux_arm64` | ARMv8-A, glibc 2.35 |
| ARMv7 (32-bit hard-float) | `linux_armv7` | Armv7-A with VFPv3-D16 and Thumb-2, glibc 2.35 |

A Raspberry Pi 2 running Raspberry Pi OS Lite (32-bit) uses `linux_armv7`.
Choose the archive for the installed userland; a 64-bit kernel can run a
32-bit userland.

Both targets passed [earlier hosted CI](https://github.com/sandover/plasmite/actions/runs/36803923793),
including extracted CLI/library checks and HTTPS recovery. ARMv7 uses QEMU
emulation. Physical Pi installation and reboot checks remain pending.

See the [ARM installation guide](docs/record/distribution.md#linux-arm-sdk-preview-install)
for published downloads, checksums, and setup. ARMv7 pools can be at most
2,147,483,647 bytes (2 GiB minus one byte). ARMv6 is unsupported.

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

PyPI ships pre-built native bindings on macOS and Windows x86_64. Linux
x86_64 users can install the system SDK through Homebrew or a GitHub release
tarball. Linux ARM users can use the preview SDK archives. See the
[distribution guide](docs/record/distribution.md#install-matrix) for the current matrix.

### Node

Requires **Node.js 24 or newer**, including for the npm CLI.

```bash
npm i -g plasmite
```

The package includes pre-built native bindings for macOS, Linux x86_64, and
Windows x86_64. Linux ARM has no published npm native addon or CLI; use the
SDK preview archives for its native CLI/server. Installing that SDK does not
add a Node addon.

### Go

```bash
go get github.com/sandover/plasmite/bindings/go/local
```

Bindings only (no CLI). Links against `libplasmite` via cgo, so first get the SDK via Homebrew on macOS, or from a [GitHub Releases](https://github.com/sandover/plasmite/releases) tarball on Linux.

### Pre-built binaries

Tarballs for Linux and macOS are on [GitHub Releases](https://github.com/sandover/plasmite/releases). Each archive contains `bin/`, `lib/`, `include/`, and `lib/pkgconfig/`.

Windows builds (`x86_64-pc-windows-msvc`) are available via npm and PyPI. See the [distribution docs](docs/record/distribution.md) for the full install matrix.

Updating an earlier installation? Read the [1.2 upgrade guide](docs/record/upgrading-1.2.md)
for message input defaults and output formats, and the
[1.0 upgrade guide](docs/record/upgrading-1.0.md) for secure-sharing migration.

## Command Overview

**Messaging**

| | |
|---|---|
| `feed` *pool* *object* | Send a message |
| `follow` *pool* | Follow messages |
| `fetch` *pool* *seq* | Fetch one message by sequence number |
| `duplex` *pool* | 2-way session with a pool |
| `tap` *pool* `--` *command…* | Capture a process's output |

**Pool management**

| | |
|---|---|
| `pool create` *name* | Create a pool |
| `pool list` [*server*] | List local pools or a remote server's pools |
| `pool info` *pool* | Show local or remote pool metadata and metrics |
| `pool delete` *name…* | Delete one or more pools |
| `doctor` *pool* ǀ `--all` | Validate pool integrity |

**Server**

| | |
|---|---|
| `serve` [*server*] | Serve local pools over loopback HTTP and remote HTTPS |
| `serve install` [*server*] | Install the server as a boot service |
| `serve start/stop/restart/uninstall` | Control the installed server for `--dir` |
| `serve logs` | Read the installed server log |
| `serve status` [`--all`] | List live servers, or include installed stopped setups |
| `access invite` *name* | Create an access key for another client |
| `access connect` | Verify a server and save its access key |
| `access list` | List saved server destinations |
| `access status` | Check a saved server connection |
| `access disconnect` | Forget a saved connection locally |
| `access revoke` | Withdraw a server key's access |

**Agent and CLI support**

| | |
|---|---|
| `mcp` | Run local Model Context Protocol over stdin/stdout |
| `version` | Print version information |
| `completion` *shell* | Generate shell completion |

`pls` and `plasmite` are the same binary. See the [CLI guide](docs/cli.md) for pool references, input and output modes, and exit behavior.

## How it works

A pool is a single `.plasmite` file containing a persistent ring buffer:

- **Multiple writers** append concurrently (serialized via OS file locks)
- **Multiple readers** follow concurrently; shared file locks protect each message snapshot
- **Bounded retention** — old messages overwritten when full (default 1 MB, configurable)
- **Committed frames** keep unfinished writes out of readers’ results; choose flush durability when writes must reach disk before success

Every message carries a **seq** (monotonic), a **time** (nanosecond precision), optional **tags**, and your JSON **data**. Tags and `--where` (jq predicates) compose for filtering. See [Live Event Stream](docs/cookbook.md#live-event-stream).

Default pool directory: `~/.plasmite/pools/`.

## Performance

Payloads use [Lite3](https://github.com/fastserial/lite3), a binary JSON encoding.
Readers take owned message snapshots so a concurrent write cannot alter a message
while they inspect it.

The file is memory-mapped. Reads copy a validated message under a shared file
lock, then inspect the immutable snapshot after releasing the lock. Lite3
supports field lookup without decoding the full payload into JSON.

Writers encode before taking the exclusive file lock, then place the frame,
commit it, and publish the new bounds. An inline sequence index maps sequence
numbers to byte offsets. `fetch POOL 42` jumps directly to the frame when the
index slot matches; a missing or stale slot falls back to scanning retained history.

Each message uses 72–79 bytes of framing beyond its Lite3 payload: a 64-byte
header, an 8-byte commit marker, and padding for alignment.

Throughput depends on payload size, durability, readers, writers, and the host.
Run [`scripts/bench_runtime_lanes.sh`](scripts/bench_runtime_lanes.sh) for a
reproducible local measurement. The earlier lock-free read measurements do not
describe the 1.0 snapshot implementation.

See the [1.0 performance measurements](docs/performance/1.0-release.md) for
workloads, results, and their limits.
See the [Windows access measurements](docs/performance/windows-access-1.0.md)
for local, HTTP, and MCP costs and verified Codex CLI and Claude Code access.

## More

**Specs**: [CLI](spec/v0/SPEC.md) | [API](spec/api/v0/SPEC.md) | [Remote protocol](spec/remote/v0/SPEC.md)

**Bindings**: [Go](bindings/go/README.md) | [Python](bindings/python/README.md) | [Node](bindings/node/README.md)

**Guides**: [CLI](docs/cli.md) | [Serving & remote access](docs/record/serving.md) | [Distribution](docs/record/distribution.md)

**Contributing**: See `AGENTS.md` for CI hygiene; `docs/record/releasing.md` for release process


[Changelog](CHANGELOG.md) | Inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma).

## License

MIT. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for vendored code.
