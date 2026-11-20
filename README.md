# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![Homebrew Tap](https://img.shields.io/badge/Homebrew-Tap-2EA44F?logo=homebrew&logoColor=white)](https://github.com/sandover/homebrew-tap)
[![crates.io](https://img.shields.io/crates/v/plasmite?logo=rust)](https://crates.io/crates/plasmite)
[![PyPI](https://img.shields.io/pypi/v/plasmite?logo=pypi)](https://pypi.org/project/plasmite/)
[![npm](https://img.shields.io/npm/v/plasmite?logo=npm)](https://www.npmjs.com/package/plasmite)
[![Go Reference](https://pkg.go.dev/badge/github.com/sandover/plasmite/bindings/go/v2/local.svg)](https://pkg.go.dev/github.com/sandover/plasmite/bindings/go/v2/local)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**Easy interprocess communication.**

What would make it pleasant and predictable for programs (and agents) to communicate?

- Programs come and go... so **message channels should outlast them**
- Machines crash... so **channels should live on disk**
- Disks are finite... so **channels should have a bounded size**
- Brokers bring complexity and ceremony... so **local messaging shouldn't need one**
- People need to see what's happening... so **messages should be plain JSON**
- Schemas are great... but **schemas should be optional**

So, there's **Plasmite**.

Programs and scripts (shell, Rust, C, Python, Go or JS) send JSON messages into a channel called a *pool*. A pool is a ring buffer on disk: it keeps the most recent messages and overwrites the oldest when it fills. Three verbs, in the CLI and every library:

- `feed` sends a message
- `fetch` reads what's already in the pool, then stops
- `follow` waits for new messages (like `tail -f`)

On the command line, `pls` is short for `plasmite`.

#### Local message passing

Read down: each row happens after the one above.

| | Alice in Terminal 1 | Alice in Terminal 2 |
|---|---|---|
| 1 | **Creates a message pool**<br>`pls pool create channel` | |
| 2 | | **Starts watching**<br>`pls follow channel` |
| 3 | **Sends a message**<br>`pls feed channel '{"msg":"hello world"}'` | **Sees it**<br>`{"data":{"msg":"hello world"}, ...}` |

The message stays in the pool, so a later reader can fetch it: `pls fetch channel --last 1`.

#### Remote message passing

Remote pools take the same commands as local ones.

| | Alice in Terminal 1 | Alice in Terminal 2 | Bob (another machine) |
|---|---|---|---|
| 4 | **Starts the server** in the background<br>`pls serve https://alice.local:9743 &` | | |
| 5 | **Invites Bob**, then sends him the address and key<br>`pls access invite Bob` | | |
| 6 | | | **Connects, pastes in the key, and then watches**<br>`pls access connect https://alice.local:9743`<br>`pls follow https://alice.local:9743/channel` |
| 7 | **Sends**<br>`pls feed channel '{"msg":"hi all"}'` | **Sees it**<br>`{"data":{"msg":"hi all"}, ...}` | **Sees it**<br>`{"data":{"msg":"hi all"}, ...}` |

Bob can also browse the pools at `https://alice.local:9743` once his browser trusts Alice's certificate. See the [serving guide](docs/record/serving.md).

#### Agents

Agents from any vendor can share a pool: Claude Code on Alice's laptop, Codex on Bob's build box, a script in CI. Alice's server speaks MCP, so a remote agent needs nothing installed. Add the server, then sign in with an access key:

```bash
claude mcp add --transport http plasmite https://alice.local:9743/mcp
codex mcp add plasmite --url https://alice.local:9743/mcp
```

Alice's agent posts "tests pass on `fix-login`". Bob's agent is waiting on the pool and wakes the moment the message lands, without polling. An agent that restarts with a fresh context reads the pool to catch up on what it missed. Alice and Bob watch the whole exchange with `pls follow channel`, and can step in with `pls feed`.

Each agent's machine needs to trust Alice's certificate. See [Plasmite for agents](docs/agents.md).

#### Fire-and-forget over UDP

On a trusted network, Alice's server can also take UDP datagrams. A UDP sender fires off a message and moves on, where HTTP waits for the server to confirm the write. That suits sensors and other chatty senders that can live with an occasional lost message.

```bash
kill %1 && pls serve https://alice.local:9743 --udp=0.0.0.0:9701 &   # Alice restarts her server with UDP on
pls feed udp://alice.local:9701/channel '{"temp_c": 21.4}'              # Bob
```

## How it works

A pool is a `.plasmite` file in `~/.plasmite/pools/` by default. Any number of programs can feed and follow the same pool.

Every message has four fields:

- `data`: your JSON payload
- `meta.tags`: optional text labels you attach with `--tag`, for cheap filtering
- `seq`: message index, counting up from 1
- `time`: when the pool stored it

`fetch` and `follow` can start wherever:

```bash
pls fetch channel --last 5         # the last 5 messages, then stop
pls follow channel --since 10m     # the last 10 minutes, then keep waiting
```

**Pipes just work.** Pipe `fetch` or `follow` into `feed` to copy messages between pools, tags and all. `jq` and similar tools work great.

## Comparison with other styles of IPC

| If you'd reach for | What gets in the way | With Plasmite |
|---|---|---|
| **Kafka**, **RabbitMQ** | Lots to set up before the first message: brokers, partitions, consumer groups | No config — just make a pool & go |
| **Redis**, **NATS** | Always needs a server; messages lost on restart unless configured | Local pools don't need a server; messages survive restarts |
| **Log files and `tail -f`** | A wall of text, growing without limits | Numbered series of JSON objects, fixed-size file |
| **Temp files and watched folders** | Polling loops, stale lock files after a crash, files piling up | No polling, no lock files, nothing to clean up |
| **SQLite as a queue** | Your own table, cleanup job and polling loop | Following, replay and bounded history built in |
| **Pipes and sockets** | One reader & no memory | Many readers and writers; late readers catch up |
| **ZeroMQ** | Messages gone on restart, in frames ordinary tools can't read | Messages survive restarts, as JSON you can pipe through `jq` |

**Use cases**: agent handoffs, CI gates, live event streams, sensor feeds, duplex chat, system log ring buffers, replay & debug. See the **[Cookbook](docs/cookbook.md)**.

## Performance

Fast, small and quiet. Measured through the local APIs on an Apple M5 Pro Mac, with a 2.0 development build and medians of seven runs:

| Language | Publish small messages (msg/s) | Peak memory while publishing |
|---|---:|---:|
| Rust | 404,900 | 17 MiB |
| C | 309,200 | 18 MiB |
| Go | 213,800 | 28 MiB |
| Python | 90,600 | 40 MiB |
| JavaScript (Node) | 84,800 | 67 MiB |

Delivery is quick: at 250 messages per second with 1 KiB payloads, a message reached **one reader in 0.13 ms** and **four readers in 0.19 ms** (median p95), and every reader got every message. A reader waiting on a quiet pool uses **under 0.1% of one CPU core**.

See the [live-stream measurements](docs/performance/language-boundary-results/wakeups/README.md#live-stream-with-one-and-four-readers) and the [full 2.0 benchmarks](CHANGELOG.md#faster-publishing-and-catch-up), including replay.

## Install

### macOS and Linux (Homebrew)

```bash
brew install sandover/tap/plasmite
```

Installs the CLI (`plasmite` + `pls`) and the full SDK (`libplasmite`, C header, pkg-config). Go links against this SDK, so install it first if you use Go.

### Python

```bash
uv tool install plasmite   # CLI
uv add plasmite            # library, in a uv project
```

Wheels for macOS, Linux x86_64 and Windows x86_64 bundle the CLI and native library.

### Node

```bash
npm i -g plasmite          # CLI
npm i plasmite             # library
```

Needs Node.js 24 or newer, on macOS, Linux x86_64 or Windows x86_64.

### Rust

```bash
cargo install plasmite     # CLI
cargo add plasmite         # library
```

### Go

```bash
go get github.com/sandover/plasmite/bindings/go/v2/local@v2.0.0
```

### Everything else

[GitHub Releases](https://github.com/sandover/plasmite/releases) has archives of the CLI and SDK for macOS and Linux, including preview builds for Linux ARM and Raspberry Pi. See the [distribution guide](docs/record/distribution.md) for every platform.

Upgrading from 1.x? See the [2.0 upgrade guide](docs/record/upgrading-2.0.md).

## Learn more

`pls --help` lists every command; `pls COMMAND --help` lists its options.

- [Cookbook](docs/cookbook.md): task-by-task recipes.
- [Plasmite for agents](docs/agents.md): connecting agents, handoffs and instructions to paste into `AGENTS.md`.
- [Performance reports](docs/performance/README.md): local, HTTP, UDP and MCP measurements.
- [CLI guide](docs/cli.md): pool references, input and output formats, exit codes.
- Libraries: [Rust](README-crate.md), [Python](bindings/python/README.md), [Node](bindings/node/README.md), [Go](bindings/go/README.md), [C](include/plasmite.h).
- [Serving guide](docs/record/serving.md): HTTPS, access keys, browsers, MCP, UDP, running at boot.
- [Specification](spec/v2/SPEC.md): the exact contract for the CLI, libraries, HTTP, UDP, and MCP.
- [Changelog](CHANGELOG.md). Contributors: see `AGENTS.md` and the [release process](docs/record/releasing.md).

Inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma).

## License

MIT. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for vendored code.
