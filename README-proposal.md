# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Easy interprocess communication.

```bash
pls poke chat --create '{"from": "alice", "msg": "hello"}'
pls poke chat '{"from": "alice", "msg": "you there?"}'

# In another terminal:
pls peek chat
#   {"seq":1,"time":"...","meta":{"tags":[]},"data":{"from":"alice","msg":"hello"}}
#   {"seq":2,"time":"...","meta":{"tags":[]},"data":{"from":"alice","msg":"you there?"}}
```

Plasmite gives your programs a shared conversation space — persistent message pools backed by plain files. No daemon, no broker, no config. One process pokes, another peeks, and they're talking.

~600k messages/sec on a laptop. Crash-safe writes. Ring buffers that never block.

## Install

```bash
brew install sandover/tap/plasmite    # macOS
cargo install plasmite                # anywhere with Rust
```

Installs both `plasmite` and the `pls` shorthand.

Prefer manual binaries? Grab your platform tarball from the
[v0.1.0 release](https://github.com/sandover/plasmite/releases/tag/v0.1.0).

## Why not just...

| | The catch | With Plasmite |
|---|---|---|
| **Temp files + locks** | No watching, collisions everywhere | Multiple writers, real-time streaming |
| **Named pipes** | Blocks writers, single reader | Non-blocking, multiple readers |
| **Poll a directory** | Busy loops, no ordering | Sequence numbers, streaming |
| **Redis** | Daemon to run and feed | Just files |
| **WebSockets** | Server to build and deploy | No networking needed |

Pools are regular files — `ls` them, `rm` them, back them up with `cp`. Messages are JSON, so pipe through `jq`. Nothing to install on the reading side except Plasmite itself.

## What it looks like

### Two terminals, two processes, one pool

**Terminal 1** — your build script writes progress:
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

## Performance

| Metric | Number |
|---|---|
| Append throughput | ~600k msg/sec (single writer, M1 MacBook) |
| Read throughput | Lock-free, zero-copy |
| Message overhead | 48 bytes per message |
| Default pool size | 1 MB (~20k messages) |
| Max tested pool | 1 GB |

Pools are memory-mapped ring buffers. Writers serialize through OS file locks; readers never block. Old messages are overwritten when the ring is full — bounded retention by design, not by accident.

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

## Commands

| Command | What it does |
|---|---|
| `poke POOL DATA` | Send a message (`--create` to auto-create the pool) |
| `peek POOL` | Watch messages (streams until Ctrl-C) |
| `get POOL SEQ` | Fetch one message by sequence number |
| `pool create NAME` | Create a pool (`--size 8M` for larger) |
| `pool list` | List pools |
| `pool info NAME` | Show pool metadata and metrics |
| `pool delete NAME...` | Delete one or more pools |
| `doctor POOL \| --all` | Validate pool integrity |
| `serve` | HTTP server (loopback default; non-loopback opt-in) |

`pls` and `plasmite` are the same binary. Shell completion: `plasmite completion bash|zsh|fish`.

## How it works

A pool is a single `.plasmite` file containing a persistent ring buffer:

- **Multiple writers** append concurrently (serialized via OS file locks)
- **Multiple readers** watch concurrently (lock-free, zero-copy)
- **Bounded retention** — old messages overwritten when full (default 1 MB, configurable)
- **Crash-safe** — torn writes never propagate

Every message carries a **seq** (monotonic), a **time** (nanosecond precision), optional **tags**, and your JSON **data**. Tags and `--where` (jq predicates) compose for filtering. See the [pattern matching guide](docs/record/pattern-matching.md).

Default pool directory: `~/.plasmite/pools/`.

## More

**Specs**: [CLI](spec/v0/SPEC.md) | [API](spec/api/v0/SPEC.md) | [Remote protocol](spec/remote/v0/SPEC.md)

**Guides**: [Rust API quickstart](docs/record/api-quickstart.md) | [Go quickstart](docs/record/go-quickstart.md) | [libplasmite C ABI](docs/record/libplasmite.md) | [Exit codes](docs/record/exit-codes.md) | [Diagnostics](docs/record/doctor.md)

[Changelog](CHANGELOG.md) | Inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma).

## License

MIT. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for vendored code.
