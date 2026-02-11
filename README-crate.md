# plasmite

Persistent JSON message queues backed by plain files. No daemon, no broker, no
config.

Plasmite gives you fast, crash-safe, disk-backed ring buffers ("pools") that
multiple processes can read and write concurrently. Use it for IPC, event
sourcing, job queues, or anywhere you'd reach for Redis or a database-backed
queue but don't want to run a server.

- ~600k msg/sec append throughput (single writer, M3 MacBook)
- Lock-free, zero-copy reads via mmap
- Crash-safe writes with configurable durability
- Bounded disk usage (ring buffer — old messages overwritten when full)
- Structured JSON messages with sequence numbers, timestamps, and tags

## Quick start

```rust
use plasmite::api::{
    Durability, LocalClient, PoolApiExt, PoolOptions, PoolRef, TailOptions,
};
use serde_json::json;

// Create a client (pools stored in ~/.plasmite/pools/ by default)
let client = LocalClient::new();

// Create a 1 MB pool
let pool_ref = PoolRef::name("events");
client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
let mut pool = client.open_pool(&pool_ref)?;

// Append messages with tags
let msg = pool.append_json_now(
    &json!({"kind": "signup", "user": "alice"}),
    &["user-event".into()],
    Durability::Fast,
)?;
println!("seq={} time={}", msg.seq, msg.time);

// Read back by sequence number
let fetched = pool.get_message(1)?;
assert_eq!(fetched.data["user"], "alice");

// Tail — stream messages as they arrive
let mut tail = pool.tail(TailOptions {
    tags: vec!["user-event".into()],
    ..TailOptions::default()
});
while let Some(message) = tail.next_message()? {
    println!("{}: {}", message.seq, message.data);
}
```

## Core concepts

A **pool** is a single `.plasmite` file containing a ring buffer. Messages
are appended to the head and the oldest messages are silently overwritten when
the pool is full.

Every message carries:
- **seq** — monotonically increasing sequence number
- **time** — nanosecond-precision UTC timestamp
- **tags** — optional string labels for filtering
- **data** — your JSON payload

Multiple processes can write to the same pool concurrently (serialized via OS
file locks). Multiple processes can read concurrently (lock-free).

## API overview

### Client and pool lifecycle

```rust
use plasmite::api::{LocalClient, PoolRef, PoolOptions};

let client = LocalClient::new();
// Or with a custom directory:
let client = LocalClient::new().with_pool_dir("/tmp/my-pools");

// Create
client.create_pool(&PoolRef::name("logs"), PoolOptions::new(64 * 1024 * 1024))?;

// Open (returns a mutable Pool handle)
let mut pool = client.open_pool(&PoolRef::name("logs"))?;

// Inspect
let info = client.pool_info(&PoolRef::name("logs"))?;
println!("bounds: {:?}", info.bounds);

// List all pools
let pools = client.list_pools()?;

// Delete
client.delete_pool(&PoolRef::name("logs"))?;
```

Pool references resolve names to `~/.plasmite/pools/{name}.plasmite`, or
you can use `PoolRef::path(...)` for an absolute path.

### Writing messages

The `PoolApiExt` trait extends `Pool` with the message API:

```rust
use plasmite::api::{PoolApiExt, Durability, AppendOptions};
use serde_json::json;

// Simple append (generates timestamp for you)
let msg = pool.append_json_now(
    &json!({"temp": 23.5}),
    &["sensor".into()],
    Durability::Fast,
)?;

// With explicit options (custom timestamp)
let msg = pool.append_json(
    &json!({"temp": 24.1}),
    &["sensor".into()],
    AppendOptions::new(1_700_000_000_000_000_000, Durability::Flush),
)?;
```

**Durability:**
- `Durability::Fast` — buffered writes, higher throughput
- `Durability::Flush` — fsync after write, crash-safe

### Reading messages

```rust
use plasmite::api::PoolApiExt;

// By sequence number
let msg = pool.get_message(42)?;
println!("{}: {} {:?}", msg.seq, msg.data, msg.meta.tags);
```

### Tailing (streaming)

```rust
use plasmite::api::{PoolApiExt, TailOptions};
use std::time::Duration;

let mut tail = pool.tail(TailOptions {
    since_seq: Some(100),                  // start after seq 100
    max_messages: Some(50),                // stop after 50
    timeout: Some(Duration::from_secs(5)), // stop after 5s idle
    tags: vec!["important".into()],        // filter by tag
    ..TailOptions::default()
});

while let Some(msg) = tail.next_message()? {
    println!("{}", msg.data);
}
```

### Replay

Play back messages with timing preserved:

```rust
use plasmite::api::{PoolApiExt, ReplayOptions};

let mut replay = pool.replay(ReplayOptions::new(10.0))?; // 10x speed
while let Some(msg) = replay.next_message() {
    println!("{}: {}", msg.time, msg.data);
}
```

### Remote pools

Connect to a plasmite server over HTTP:

```rust
use plasmite::api::{RemoteClient, PoolRef, Durability};
use serde_json::json;

let client = RemoteClient::new("http://127.0.0.1:9700")?
    .with_token("my-secret-token");

let pool = client.open_pool(&PoolRef::name("events"))?;
let msg = pool.append_json_now(
    &json!({"kind": "deploy"}),
    &["ops".into()],
    Durability::Fast,
)?;

// Tail remote messages
let mut tail = pool.tail(Default::default())?;
while let Some(msg) = tail.next_message()? {
    println!("{}", msg.data);
}
tail.cancel();
```

### Error handling

Errors carry structured context:

```rust
use plasmite::api::ErrorKind;

match pool.get_message(9999) {
    Ok(msg) => println!("{}", msg.data),
    Err(e) if e.kind() == ErrorKind::NotFound => {
        eprintln!("no message at seq 9999");
    }
    Err(e) => {
        // e.hint(), e.path(), e.seq(), e.offset() available
        return Err(e);
    }
}
```

Error kinds: `Internal`, `Usage`, `NotFound`, `AlreadyExists`, `Busy`,
`Permission`, `Corrupt`, `Io`.

### Pool validation

```rust
let report = client.validate_pool(&PoolRef::name("events"))?;
match report.status {
    plasmite::api::ValidationStatus::Ok => println!("pool healthy"),
    plasmite::api::ValidationStatus::Corrupt => {
        for issue in &report.issues {
            eprintln!("{}: {}", issue.code, issue.message);
        }
    }
}
```

### Lite3 (binary framing)

For high-throughput paths that skip JSON encoding:

```rust
use plasmite::api::{PoolApiExt, Durability};

let seq = pool.append_lite3_now(b"raw payload bytes", Durability::Fast)?;
let frame = pool.get_lite3(seq)?;
// frame.seq, frame.timestamp_ns, frame.payload
```

## CLI

The crate also installs the `plasmite` and `pls` CLI binaries:

```bash
cargo install plasmite

pls poke events --create '{"kind": "signup", "user": "alice"}'
pls peek events
pls serve
```

## Language bindings

Plasmite also has native bindings for
[Node.js](https://www.npmjs.com/package/plasmite),
[Python](https://pypi.org/project/plasmite/), and
[Go](https://github.com/sandover/plasmite/tree/main/bindings/go) — all
through the same C ABI, so pools are interoperable across languages.

## License

MIT
