# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Persistent JSON message queues backed by plain files, making interprocess communication easy and inspectable. Multiple processes can write and read concurrently. Message queues are ring buffers, so writes almost always succeed.

Use Plasmite via the CLI, native bindings (Go, Python, Node), or the HTTP API.

```bash
# First, create a pool
pls pool create chat

# Bob's terminal - Bob starts watching (blocks, waiting for messages)
pls peek chat

# Alice's terminal - Alice sends messages
pls poke chat '{"from": "alice", "msg": "hello bob"}'
pls poke chat '{"from": "alice", "msg": "you there?"}'

# Bob's Terminal - Bob sees each message appear as Alice sends it:
#   {"seq":1,"time":"...","meta":{"descrips":[]},"data":{"from":"alice","msg":"hello bob"}}
#   {"seq":2,"time":"...","meta":{"descrips":[]},"data":{"from":"alice","msg":"you there?"}}
```

No daemon, no config. ~600k messages/sec on a laptop.

## Why Plasmite?

Common local IPC options have tradeoffs:
- Temp files need locking and don't support watching
- Named pipes block writers and allow only one reader
- Redis/RabbitMQ require running a daemon
- WebSockets require networking code

Plasmite pools are ring buffers that support multiple concurrent writers and readers. Messages are JSON, so you can filter with `jq`. Pools are regular files you can `ls` and `rm`. Writes are crash-safe.

| Alternative | Limitation | Plasmite |
|-------------|------------|----------|
| Temp files + locks | No watching, collision-prone | Multiple writers, real-time reads |
| Named pipes | Blocks writers, one reader | Non-blocking, multiple readers |
| Polling a directory | Busy loops, no ordering | Streaming with sequence numbers |
| Redis | Requires daemon | Just files |
| WebSockets | Requires server | No networking |

## Install

```bash
cargo install --path . --locked
```

This installs `plasmite` and the `pls` alias. Supported: **macOS** and **Linux**.

## Examples

### Watch a process from another terminal

**Terminal 1** - your build script:
```bash
pls poke build --create '{"step": "compile", "status": "running"}'
sleep 2  # ... compiling ...
pls poke build '{"step": "compile", "status": "done"}'
pls poke build '{"step": "test", "status": "running"}'
```

**Terminal 2** - you, watching:
```bash
pls peek build
```

### Coordinate two scripts

```bash
# Script A waits for a signal
echo "Waiting for go signal..."
pls peek signals --where '.data.go == true' --tail 1 --one > /dev/null
echo "Got it! Proceeding..."

# Script B sends the signal
pls poke signals --create '{"go": true}'
```

### Consume (or combine) streams from different sources

```bash
# Pipe JSON Lines from jq
jq -c '.items[]' data.json | pls poke foo --create

# Stream from curl (event streams auto-detected)
curl -N https://api.example.com/stream | pls poke events --create

# System logs (Linux)
journalctl -o json-seq -f | pls poke syslog --create

# System logs (macOS)
/usr/bin/log stream --style ndjson | pls poke syslog --create
```

### Filter and transform

```bash
# Only errors
pls peek foo --where '.data.level == "error"'

# Only messages tagged "important"
pls peek foo --where '.meta.descrips[]? == "important"'

# Pipe to jq for transformation
pls peek foo --format jsonl | jq -r '.data.msg'

# Last 10 messages, then keep watching
pls peek foo --tail 10

# Messages from the last 5 minutes
pls peek foo --since 5m
```

### Use it from scripts

**Via CLI** (any language):
```bash
pls poke foo --create '{"from": "bash"}'
```

**Native bindings** (no subprocess overhead):

```go
// Go
client, _ := plasmite.NewClient("./data")
pool, _ := client.CreatePool(plasmite.PoolRefName("foo"), 1024*1024)
pool.Append(map[string]any{"from": "go"}, nil, plasmite.DurabilityFast)
```

```python
# Python
from plasmite import Client
client = Client("./data")
pool = client.create_pool("foo", 1024*1024)
pool.append_json(b'{"from": "python"}', [], "fast")
```

```javascript
// Node.js
const { Client } = require("plasmite-node")
const client = new Client("./data")
const pool = client.createPool("foo", 1024 * 1024)
pool.appendJson(Buffer.from('{"from": "node"}'), [], "fast")
```

See [Go quickstart](docs/go-quickstart.md), [bindings/python](bindings/python/README.md), and [bindings/node](bindings/node/README.md) for full documentation.

## Commands

| Command | Description |
|---------|-------------|
| `poke POOL DATA` | Send a message (`--create` to auto-create pool) |
| `peek POOL` | Watch messages (streams until Ctrl-C) |
| `get POOL SEQ` | Fetch one message by seq number |
| `pool create NAME` | Create a pool (`--size 8M` for larger) |
| `pool list` | List pools |
| `pool info NAME` | Show pool metadata |
| `pool delete NAME` | Delete a pool |
| `doctor POOL` | Validate pool health (`--all` for all pools) |
| `serve` | Serve pools over HTTP (loopback default; non-loopback opt-in) |

Both `pls` and `plasmite` commands are supported.

## How It Works

### Pools

A pool is a **persistent ring buffer** - one `.plasmite` file:

- **Multiple writers** append concurrently (serialized via OS file locks)
- **Multiple readers** watch concurrently (lock-free, zero-copy)
- **Bounded retention** - old messages overwritten when full
- **Crash-safe** - torn writes never propagate

Default location: `~/.plasmite/pools/`. Create explicitly or use `--create` on first poke.

### Messages

```json
{
  "seq": 42,
  "time": "2026-02-03T12:00:00.123Z",
  "meta": { "descrips": ["error", "db"] },
  "data": { "your": "payload" }
}
```

- **seq** - auto-incrementing ID (for ordering, deduplication, `plasmite get`)
- **time** - when it was written (RFC 3339, nanosecond precision)
- **meta.descrips** - tags you add with `--descrip` (for filtering with `--where`)
- **data** - your JSON payload

Tag messages when you poke them:
```bash
pls poke foo --descrip error --descrip db '{"msg": "connection lost"}'
```

Filter by tag when you peek:
```bash
pls peek foo --where '.meta.descrips[]? == "error"'
```

### Scripting

Plasmite is built for scripts:
- **TTY**: Human-readable errors with hints
- **Pipes**: JSON errors on stderr, stable exit codes
- See [docs/exit-codes.md](docs/exit-codes.md) for the full list

## Advanced

### Pool size

Default is 1MB. Old messages are overwritten when full:

```bash
pls pool create bigpool --size 64M    # More history
pls poke signals --create --create-size 64K '...'  # Tiny, ephemeral
```

### Durability

Default is fast (OS buffered). For critical data:

```bash
pls poke foo --durability flush '{"important": true}'
```

### Retries

Handle lock contention gracefully:

```bash
pls poke foo --retry 3 --retry-delay 100ms '{"data": 1}'
```

### Input modes

`poke` auto-detects JSONL, JSON-seq (0x1e), and event streams. Override with `--in`:

```bash
cat records.jsonl | pls poke foo --in jsonl
cat big.json | pls poke foo --in json
```

Skip bad records with `--errors skip` (exit 1 if any skipped).

## Remote Access

Serve pools over HTTP for access from other machines or containers.

### Local development

```bash
pls serve
# Listening on 127.0.0.1:9700
```

That's it. Now other processes on this machine can read and write:

```bash
# Append via CLI (shorthand URL)
pls poke http://127.0.0.1:9700/demo '{"msg": "hello"}'

# Tail via curl (streams until Ctrl-C)
curl -N http://127.0.0.1:9700/v0/pools/demo/tail
```

### Exposing to LAN

Non-loopback binds require explicit opt-in and security:

```bash
# Read-only (safe for dashboards, no auth required)
pls serve --bind 0.0.0.0:9700 --allow-non-loopback --access read-only

# Read-write with TLS + token auth
pls serve --bind 0.0.0.0:9700 --allow-non-loopback \
  --token-file ~/.plasmite/token \
  --tls-cert /path/to/cert.pem --tls-key /path/to/key.pem
```

### From code

```javascript
// Node.js
const { RemoteClient } = require("plasmite-node")
const client = new RemoteClient("http://127.0.0.1:9700")
const pool = await client.openPool("demo")
await pool.append({ msg: "hello" }, ["remote"])
```

> **Notes:** Remote `poke` uses shorthand URLs (`http://host:port/pool`). Pool creation is local-only by design. See [Remote protocol spec](spec/remote/v0/SPEC.md) for the full API.

## Performance

Benchmarks on a laptop (M-series Mac, release build, 256-byte payloads):

- **Append**: 600k+ messages/sec (single writer, fast durability)
- **Follow latency**: sub-millisecond typical, ~3ms worst case
- **Concurrent writers**: scales to 8+ with graceful degradation

See [docs/perf.md](docs/perf.md) for the full benchmark suite.

## Internals

Plasmite uses **[Lite³](https://github.com/fastserial/lite3)** (a zero-copy binary JSON format) for on-disk storage. The CLI is JSON-in/JSON-out - you never see Lite³ directly.

Inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma).

## More Info

**Specs** (normative contracts):
- [CLI spec](spec/v0/SPEC.md) - Stable contract for scripting
- [API spec](spec/api/v0/SPEC.md) - Public API for bindings
- [Remote spec](spec/remote/v0/SPEC.md) - HTTP/JSON remote protocol

**Guides**:
- [API quickstart](docs/api-quickstart.md) - Embedding in Rust
- [Go quickstart](docs/go-quickstart.md) - Using the Go bindings
- [libplasmite](docs/libplasmite.md) - Building the C ABI

**Reference**:
- [Architecture](docs/architecture.md) - How it's built
- [Exit codes](docs/exit-codes.md) - For robust error handling
- [Diagnostics](docs/doctor.md) - Pool validation
- [Changelog](CHANGELOG.md) - Version history

## License

MIT. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for vendored code.
