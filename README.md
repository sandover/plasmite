# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![Homebrew Tap](https://img.shields.io/badge/Homebrew-Tap-2EA44F?logo=homebrew&logoColor=white)](https://github.com/sandover/homebrew-tap)
[![crates.io](https://img.shields.io/crates/v/plasmite?logo=rust)](https://crates.io/crates/plasmite)
[![PyPI](https://img.shields.io/pypi/v/plasmite?logo=pypi)](https://pypi.org/project/plasmite/)
[![npm](https://img.shields.io/npm/v/plasmite?logo=npm)](https://www.npmjs.com/package/plasmite)
[![Go Reference](https://pkg.go.dev/badge/github.com/sandover/plasmite/bindings/go/v2/local.svg)](https://pkg.go.dev/github.com/sandover/plasmite/bindings/go/v2/local)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**Plasmite is a pipe that remembers.** Programs send and receive JSON messages through a bounded file called a pool. The pool keeps recent messages after writers exit. When it fills, new messages overwrite the oldest ones.

Three verbs cover almost everything:

- `feed` writes a message.
- `follow` waits for new messages, like `tail -f`.
- `fetch` reads what the pool holds and exits.

Local pools need no server or broker. The CLI and the Rust, Python, Go, Node and C libraries share one model, so a shell script can feed a pool that a Python service follows. `pls` is a short alias for `plasmite`.

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
      <code>pls feed channel '{"from":"A","msg":"hello world"}'</code>
    </td>
    <td valign="top">
      <b>Bob starts watching, after Alice sent</b><br/>
      <code>pls follow channel --last 1</code>
      <br/><br/>
      <b>Bob sees it on stdout, then waits for more</b><br/>
      <code>{"data":{"from":"A","msg":"hello world"}, ...}</code>
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
      <code>pls serve https://alice.example.test:9743</code>
      <br/><br/>
      <b>Alice invites Carol</b> (in another terminal)<br/>
      <code>pls access invite Carol</code>
      <br/><br/>
      She sends Carol the address and the printed key privately.
      <br/><br/>
      <b>Alice sends</b><br/>
      <code>pls feed channel '{"from":"A","msg":"hi all"}'</code>
    </td>
    <td valign="top">
      <i>Bob never quit his <code>follow</code>, so he's still watching the same pool.</i>
      <br/><br/>
      <b>Bob sees it</b><br/>
      <code>{"data":{"from":"A","msg":"hi all"}, ...}</code>
    </td>
    <td valign="top">
      <b>Carol connects and watches</b><br/>
      <code>pls access connect https://alice.example.test:9743</code><br/>
      <code>pls follow https://alice.example.test:9743/channel</code>
      <br/><br/>
      <b>Carol sees it</b><br/>
      <code>{"data":{"from":"A","msg":"hi all"}, ...}</code>
    </td>
  </tr>
</table>

Carol pastes the key at a hidden prompt. The key grants full access to Alice's pools, so treat it like a password. `feed`, `fetch`, `follow` and `duplex` work the same way on remote pools as on local ones. Carol can also open the address in a browser, AI clients can connect over the Model Context Protocol (MCP), and on a trusted network the server can accept small messages over UDP. The [serving guide](docs/record/serving.md) covers all of it.

## How it works

A pool is one `.plasmite` file holding a ring buffer, 1 MiB unless you choose a size. Pools live in `~/.plasmite/pools/` unless you pass `--dir`. Many processes can feed and follow the same pool at once, and each message arrives whole and in order.

Every message has four fields:

- `seq`: a sequence number that counts up from 1. It identifies the message.
- `time`: when the pool stored the message.
- `meta.tags`: labels you attach with `--tag`, for cheap filtering.
- `data`: your JSON object.

Every read starts at a position. Without one, `follow` waits for the next new message and `fetch` reads everything the pool holds:

```bash
pls fetch channel --last 5            # the last 5 messages, then exit
pls follow channel --since 10m        # the last 10 minutes, then keep waiting
pls follow channel --since 42         # from sequence 42 on
pls follow channel --tag deploy --one # wait for one message tagged deploy, then exit
```

A reader that falls behind never loses messages silently. If the pool has overwritten messages a reader asked for, the reader gets a `RetentionGap` error naming the first missing sequence and `next_seq`, where it can resume. `fetch` exits with the error, `follow` reports it on stderr and carries on from `next_seq`, and the libraries raise it. To resume after a restart, save the last `seq` you handled and start from the one after it.

Output is readable in a terminal and compact JSON in a pipe, so `jq` and other tools work on it directly. Piping `fetch` or `follow` into `feed` copies messages to another pool with their data and tags.

## When to use it

Plasmite fits messaging on one machine or between a few nearby ones: CI gates, live event streams, sensor feeds, two-way process communication, system log ring buffers, and replay while you debug. The [Cookbook](docs/cookbook.md) has a recipe for each.

| | Drawbacks | Plasmite |
|---|---|---|
| **Kafka**, **RabbitMQ** | Lots of machinery: partitions, groups, exchanges, bindings, oh my. | Covers the 80/20 cases: no config, no broker, no partitions, no topology. |
| **Redis / NATS** | Server required even for local messaging. Persistence and delivery depend on configuration. | Pools persist on disk independent of any process. Server only if you need one. |
| **Log files / `tail -f`** | Structure and sequence numbers depend on the log format. Logs grow and need rotation and reader coordination. Can't easily replay from a specific point. Remote access needs extra setup. | Messages have structure and sequence numbers. Disk usage is bounded. Replay retained messages. Remote access is built in. |
| **Ad-hoc files (temp files, locks, polled dirs)** | Readers have to poll for new files. Locking is manual; crashes leave a stale lock. Files accumulate. No ordering unless you bake it into filenames. | Readers stream messages. Writers append concurrently with file locks managed by the library, and messages are ordered. Ring buffer bounds disk usage. |
| **SQLite as a queue** | Queue semantics, schemas, retention and notification need application code. Sharing the database file over a network is discouraged. | Built-in follow, replay and bounded retention. Remote access through the server. |
| **OS primitives (pipes, sockets, shm)** | Named pipes mean if the reader dies, the writer blocks or gets SIGPIPE. With sockets you have to implement your own framing and reconnection. Shared memory needs coordination and crash recovery. Persistence needs extra work. | Many readers, many writers, committed messages, persistent across reboots. |
| **ZeroMQ** | Messages vanish when processes restart. The pattern matrix is expressive but hard to get right. Binary protocol means you can't inspect messages with standard tools. | Messages persist. One message model for local and remote use. Plain JSON you can pipe through `jq`. |

Plasmite keeps recent history and forgets the rest; it does not hold work until a consumer finishes it. If you need cluster replication, schema registries, or workflow orchestration, see [When Plasmite isn't the right fit](docs/cookbook.md#when-plasmite-isnt-the-right-fit).

## Install

**Homebrew** (macOS and Linux x86_64) installs the CLI and the SDK (library, C header, pkg-config):

```bash
brew install sandover/tap/plasmite
```

**Python** (macOS, Linux x86_64, Windows x86_64) installs the CLI and the Python library:

```bash
uv tool install plasmite   # CLI
uv add plasmite            # library in a uv project
```

**Node** (Node.js 24 or newer, on macOS, Linux x86_64 or Windows x86_64). Install the CLI globally or add the library to a project:

```bash
npm i -g plasmite
npm i plasmite
```

**Rust** builds from source:

```bash
cargo install plasmite     # CLI
cargo add plasmite         # library
```

**Go** links against the SDK through cgo, so install the SDK first with Homebrew or from a release archive:

```bash
go get github.com/sandover/plasmite/bindings/go/v2/local@v2.0.0
```

**Release archives** on [GitHub Releases](https://github.com/sandover/plasmite/releases) hold the CLI and SDK for macOS and Linux, including preview builds for Linux ARM and Raspberry Pi. The [distribution guide](docs/record/distribution.md) lists every platform, its requirements, and how to install the archives.

Upgrading from 1.x? Read the [2.0 upgrade guide](docs/record/upgrading-2.0.md).

## Commands

| Command | What it does |
|---|---|
| `feed POOL [MESSAGE]` | Send a JSON object, or messages from stdin |
| `follow POOL` | Wait for new messages; `--since` or `--last` starts in history |
| `fetch POOL [SEQ]` | Read retained messages, or one message by sequence, and exit |
| `duplex POOL` | Send from stdin and follow on stdout in one session |
| `pool create\|list\|info\|delete` | Manage pool files |

Run `plasmite --help` for everything else, including the server, access keys, and MCP, and `plasmite COMMAND --help` for each command's options.

## Performance

Local pools need no network round trip. The [performance reports](docs/performance/README.md) measure local, HTTP, UDP and MCP access across different payloads, read paths and hosts, with benchmarks you can rerun.

## Learn more

- [Cookbook](docs/cookbook.md): task-by-task recipes.
- [CLI guide](docs/cli.md): pool references, input and output formats, exit codes.
- Libraries: [Rust](README-crate.md), [Python](bindings/python/README.md), [Node](bindings/node/README.md), [Go](bindings/go/README.md), [C](include/plasmite.h).
- [Serving guide](docs/record/serving.md): HTTPS, access keys, browsers, MCP, UDP, running at boot.
- [Specification](spec/v2/SPEC.md): the exact contract for the CLI, libraries, HTTP, UDP, and MCP.
- [Changelog](CHANGELOG.md). Contributors: see `AGENTS.md` and the [release process](docs/record/releasing.md).

Inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma).

## License

MIT. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for vendored code.
