# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Persistent JSON message pools backed by plain files. Multiple processes write and read concurrently — no daemon, no config.

```bash
pls poke chat --create '{"from": "alice", "msg": "hello bob"}'
pls poke chat '{"from": "alice", "msg": "you there?"}'

# In another terminal:
pls peek chat
#   {"seq":1,"time":"...","meta":{"tags":[]},"data":{"from":"alice","msg":"hello bob"}}
#   {"seq":2,"time":"...","meta":{"tags":[]},"data":{"from":"alice","msg":"you there?"}}
```

~600k messages/sec on a laptop. Pools are ring buffers, so writes almost always succeed.

## Install

```bash
brew install sandover/tap/plasmite    # macOS and Linux
cargo install plasmite                # from source
```

Installs both `plasmite` and the `pls` alias.

## Why Plasmite?

| Alternative | Limitation | Plasmite |
|-------------|------------|----------|
| Temp files + locks | No watching, collision-prone | Multiple writers, real-time reads |
| Named pipes | Blocks writers, one reader | Non-blocking, multiple readers |
| Polling a directory | Busy loops, no ordering | Streaming with sequence numbers |
| Redis | Requires daemon | Just files |
| WebSockets | Requires server | No networking needed |

Pools are regular files you can `ls` and `rm`. Messages are JSON, so you can filter with `jq`. Writes are crash-safe.

## Examples

### Watch your CI from the couch

**Terminal 1** — build script:
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
# deploy.sh — wait for tests to pass
pls peek ci --where '.data.status == "green"' --one > /dev/null
echo "Tests passed, deploying..."

# test-runner.sh — signal when done
pls poke ci --create '{"status": "green", "commit": "abc123"}'
```

### Funnel streams into one place

```bash
# Ingest an API event stream
curl -N https://api.example.com/events | pls poke events --create

# Tee system logs into a pool
journalctl -o json-seq -f | pls poke syslog --create    # Linux
/usr/bin/log stream --style ndjson | pls poke syslog --create  # macOS
```

### Filter, tag, replay

```bash
# Tag messages when you write them
pls poke incidents --tag sev1 --tag billing '{"msg": "payment gateway timeout"}'

# Filter when you read
pls peek incidents --tag sev1
pls peek incidents --where '.data.msg | test("timeout")'

# Replay the last hour at 10x speed
pls peek incidents --since 1h --replay 10
```

### Go remote

```bash
pls serve                          # start the server (local-only by default)
pls serve init                     # or bootstrap TLS + token for LAN access
```

Same CLI, just pass a URL:

```bash
# From another machine
pls poke http://server:9700/events '{"sensor": "temp", "value": 23.5}'
pls peek http://server:9700/events --tail 20
```

A built-in web UI is always available at `/ui`:

![Plasmite UI pool watch](docs/images/ui/ui-pool-watch.png)

See the [remote protocol spec](spec/remote/v0/SPEC.md) for the full HTTP/JSON API.

## Bindings

Native bindings — no subprocess overhead:

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

| Command | Description |
|---------|-------------|
| `poke POOL DATA` | Send a message (`--create` to auto-create pool) |
| `peek POOL` | Watch messages (streams until Ctrl-C) |
| `get POOL SEQ` | Fetch one message by sequence number |
| `pool create NAME` | Create a pool (`--size 8M` for larger) |
| `pool list` | List pools |
| `pool info NAME` | Show pool metadata and metrics |
| `pool delete NAME...` | Delete one or more pools |
| `doctor POOL \| --all` | Validate pool integrity |
| `serve` | HTTP server (loopback default; non-loopback opt-in) |

`pls` and `plasmite` are interchangeable. Shell completion: `plasmite completion bash|zsh|fish`.

## How It Works

A pool is a **persistent ring buffer** — one `.plasmite` file:

- **Multiple writers** append concurrently (serialized via OS file locks)
- **Multiple readers** watch concurrently (lock-free, zero-copy)
- **Bounded retention** — old messages overwritten when full (default 1MB, configurable)
- **Crash-safe** — torn writes never propagate

Every message has a **seq** (auto-incrementing), a **time** (nanosecond-precision), optional **tags**, and your JSON **data**. Tags and `--where` (jq predicates) compose for filtering. See [pattern matching guide](docs/record/pattern-matching.md).

Default pool directory: `~/.plasmite/pools/`.

## More Info

**Specs**: [CLI](spec/v0/SPEC.md) · [API](spec/api/v0/SPEC.md) · [Remote protocol](spec/remote/v0/SPEC.md)

**Guides**: [Rust API quickstart](docs/record/api-quickstart.md) · [Go quickstart](docs/record/go-quickstart.md) · [libplasmite C ABI](docs/record/libplasmite.md) · [Exit codes](docs/record/exit-codes.md) · [Diagnostics](docs/record/doctor.md)

[Changelog](CHANGELOG.md) · Inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma).

## License

MIT. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for vendored code.
