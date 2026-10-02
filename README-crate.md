# plasmite

Persistent, bounded JSON message streams backed by plain files. Local use
needs no daemon or broker.

Plasmite gives you disk-backed ring buffers ("pools") that multiple processes
can read and write concurrently. Use them for IPC, recent event history,
and process output. Old messages disappear when the ring fills; Plasmite
does not acknowledge work or retain it until a consumer finishes.

- Owned message snapshots from memory-mapped pool files
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

// Read back this message through the streaming API
let mut tail = pool.tail(TailOptions {
    since_seq: Some(msg.seq),
    max_messages: Some(1),
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
file locks). Multiple processes read concurrently using shared file locks
while copying each message into an owned snapshot.

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
    since_seq: Some(100),                  // start at seq 100 (inclusive)
    max_messages: Some(50),                // stop after 50
    timeout: Some(Duration::from_secs(5)), // stop after 5s from the start
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

Use the server's credential-free loopback listener on the same machine:

```rust
use plasmite::api::{RemoteClient, PoolRef, Durability};
use serde_json::json;

let client = RemoteClient::new("http://127.0.0.1:9700")?;

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

For HTTPS sharing, run `plasmite access connect SERVER_URL` first, then
construct `RemoteClient::new(SERVER_URL)`. An application can instead supply
an in-memory access key with `RemoteClient::with_access_key(SERVER_URL, key)`.
See the [1.0 upgrade guide](https://github.com/sandover/plasmite/blob/main/docs/record/upgrading-1.0.md) for removed builders
and configuration changes.

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
`Permission`, `Corrupt`, `Io`, `RetentionGap`.

Set `TailOptions::gap_policy` to `GapPolicy::Error` to stop a tail before
delivering a message after lost history. Its `RetentionGap` error identifies the first missing sequence.
Persist the next sequence to read and choose how to recover before restarting.

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

## CLI

`cargo install plasmite` also installs the `plasmite` (and `pls`) CLI.
Local pools need no server. `plasmite serve SERVER` shares them over HTTPS;
`plasmite serve install SERVER` arranges boot startup on Linux or macOS.
See the [full README](https://github.com/sandover/plasmite) for CLI docs,
cookbook, and language bindings ([Node](https://www.npmjs.com/package/plasmite),
[Python](https://pypi.org/project/plasmite/), [Go](https://github.com/sandover/plasmite/tree/main/bindings/go)).

## License

MIT
