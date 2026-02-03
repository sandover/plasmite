# Plasmite

[![CI](https://github.com/sandover/plasmite/actions/workflows/ci.yml/badge.svg)](https://github.com/sandover/plasmite/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Persistent message pools for local IPC. Multiple processes can write and read concurrently. Messages are JSON. Pools are just files.

```bash
# First, create a pool
plasmite pool create chat

# Terminal 1 - bob starts watching (blocks, waiting for messages)
plasmite peek chat

# Terminal 2 - alice sends messages
plasmite poke chat '{"from": "alice", "msg": "hello bob"}'
plasmite poke chat '{"from": "alice", "msg": "you there?"}'

# Terminal 1 - bob sees each message appear as alice sends it:
#   {"seq":1,"time":"...","meta":{"descrips":[]},"data":{"from":"alice","msg":"hello bob"}}
#   {"seq":2,"time":"...","meta":{"descrips":[]},"data":{"from":"alice","msg":"you there?"}}
```

No daemon, no config, no ports. ~600k messages/sec on a laptop.

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
plasmite poke build --create '{"step": "compile", "status": "running"}'
sleep 2  # ... compiling ...
plasmite poke build '{"step": "compile", "status": "done"}'
plasmite poke build '{"step": "test", "status": "running"}'
```

**Terminal 2** - you, watching:
```bash
plasmite peek build
```

### Coordinate two scripts

```bash
# Script A waits for a signal
echo "Waiting for go signal..."
plasmite peek signals --where '.data.go == true' --tail 1 --one > /dev/null
echo "Got it! Proceeding..."

# Script B sends the signal
plasmite poke signals --create '{"go": true}'
```

### Ingest streams from anywhere

```bash
# Pipe JSON Lines from jq
jq -c '.items[]' data.json | plasmite poke foo --create

# Stream from curl (event streams auto-detected)
curl -N https://api.example.com/stream | plasmite poke events --create

# System logs (Linux)
journalctl -o json-seq -f | plasmite poke syslog --create

# System logs (macOS)
/usr/bin/log stream --style ndjson | plasmite poke syslog --create
```

### Filter and transform

```bash
# Only errors
plasmite peek foo --where '.data.level == "error"'

# Only messages tagged "important"
plasmite peek foo --where '.meta.descrips[]? == "important"'

# Pipe to jq for transformation
plasmite peek foo --format jsonl | jq -r '.data.msg'

# Last 10 messages, then keep watching
plasmite peek foo --tail 10

# Messages from the last 5 minutes
plasmite peek foo --since 5m
```

### Use from any language

Plasmite is just a CLI. Call it from anything:

```python
# Python
import subprocess, json
subprocess.run(["plasmite", "poke", "foo", "--create", json.dumps({"from": "python"})])
```

```javascript
// Node.js
const { execSync } = require('child_process');
execSync(`plasmite poke foo --create '${JSON.stringify({from: "node"})}'`);
```

```bash
# Or just shell
plasmite poke foo --create '{"from": "bash"}'
```

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

Alias: `pls` (e.g., `pls poke foo '{"x":1}'`)

Run `plasmite --help` or `plasmite <command> --help` for all options.

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
plasmite poke foo --descrip error --descrip db '{"msg": "connection lost"}'
```

Filter by tag when you peek:
```bash
plasmite peek foo --where '.meta.descrips[]? == "error"'
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
plasmite pool create bigpool --size 64M    # More history
plasmite poke signals --create --create-size 64K '...'  # Tiny, ephemeral
```

### Durability

Default is fast (OS buffered). For critical data:

```bash
plasmite poke foo --durability flush '{"important": true}'
```

### Retries

Handle lock contention gracefully:

```bash
plasmite poke foo --retry 3 --retry-delay 100ms '{"data": 1}'
```

### Input modes

`poke` auto-detects JSONL, JSON-seq (0x1e), and event streams. Override with `--in`:

```bash
cat records.jsonl | plasmite poke foo --in jsonl
cat big.json | plasmite poke foo --in json
```

Skip bad records with `--errors skip` (exit 1 if any skipped).

## Performance

Benchmarks on a laptop (M-series Mac, release build, 256-byte payloads):

- **Append**: 600k+ messages/sec (single writer, fast durability)
- **Follow latency**: sub-millisecond typical, ~3ms worst case
- **Concurrent writers**: scales to 8+ with graceful degradation

See [docs/perf.md](docs/perf.md) for the full benchmark suite.

## Internals

Plasmite uses **[Lite³](https://github.com/fastserial/lite3)** (a zero-copy binary JSON format) for on-disk storage. The CLI is JSON-in/JSON-out - you never see Lite³ directly.

Inspired by Oblong Industries' [Plasma](https://github.com/plasma-hamper/plasma), simplified for modern workflows.

## More Info

- [CLI spec](spec/v0/SPEC.md) - Stable contract for scripting
- [Architecture](docs/architecture.md) - How it's built
- [Exit codes](docs/exit-codes.md) - For robust error handling
- [Changelog](CHANGELOG.md) - Version history

## Development

```bash
cargo test
```

## License

MIT. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for vendored code.
