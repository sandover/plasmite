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
use plasmite::api::{Feed, LocalClient, PoolRef, Read, Since};
use serde_json::json;

let client = LocalClient::new();
let mut pool = client.open_pool(&PoolRef::name("events"), true)?;
let sent = pool.feed(&json!({"kind": "signup", "user": "alice"}), Feed::default().tags(["account"]))?;

let mut history = pool.fetch(Read::since(Since::Seq(sent.seq)))?;
let message = history.next_message()?.expect("stored message");
assert_eq!(message.data["user"], "alice");
println!("seq={} time={}", message.seq, message.time);
```

`feed` stores an object or forwards a message with its existing tags. The
destination assigns a fresh sequence and time. The time counts Unix
nanoseconds. `Feed::default()` uses buffered writes. Choose
`Feed::default().durability(Durability::Flush)` to wait for storage to flush.

`follow(Read::default())` waits for new messages. To resume a listener, save
each sequence after you finish processing it. Open
`pool.follow(Read::since(Since::Seq(last_handled + 1)))` after restart.
`pool.info()?.bounds` shows the retained range. A `RetentionGap` reports the
first lost sequence in `missing()` and where you can resume in `next_seq()`.

`fetch(Read::default())` opens retained history and stops at the last record
present at opening. Both calls accept `Since::Oldest`, `Since::Seq`,
`Since::At`, `Since::Ago`, or a `Read::last(n)` selection. Add
`.tags(["account"])` to require tags. Drop a reader when you finish, or pass
a cancellation flag to `Read::cancel` to stop a blocking wait.

### Client and pool lifecycle

```rust
use plasmite::api::{LocalClient, PoolOptions, PoolRef};

let client = LocalClient::new().with_pool_dir("./pools");
let reference = PoolRef::name("logs");
let pool = client.create_pool(&reference, PoolOptions::new(64 * 1024 * 1024))?;
println!("bounds: {:?}", pool.info()?.bounds);
let pools = client.list_pools()?;
drop(pool);
client.delete_pool(&reference)?;
```

`LocalClient::new()` resolves names in the home directory's `.plasmite/pools`.
`PoolRef::path` selects an explicit file. Pass `true` to `open_pool` to
create a missing pool at the default size. It keeps an existing size.

### Immutable snapshots and typed values

```rust
use plasmite::api::{Feed, Read, Since};
use plasmite::view::MessageView;
use serde_json::json;

let encoded = MessageView::encode(&json!({"power": 42}), &["sensor"])?;
let sent = pool.feed_view(&encoded, Feed::default())?;
let mut history = pool.fetch_view(Read::since(Since::Seq(sent.seq)))?;
let view = history.next_view()?.expect("stored view");
let power = view.data().get("power")?.unwrap().decode::<i64>()?;
let forwarded = destination.feed_view(&view, Feed::default())?;
```

A message view owns one immutable Lite3 snapshot. Typed `decode` can borrow
compatible strings and bytes. Retaining a child keeps its snapshot alive.
`follow_view` and `fetch_view` take the same read options as decoded readers.
`follow_lite3` and `fetch_lite3` yield encoded frames. Use
`feed_lite3_receipt` for the committed sequence and time alone.

UDP `follow_view` and `fetch_view` return messages alongside the server's
skip and progress events. See the [2.0 guide](docs/record/upgrading-2.0.md#keep-encoded-data-with-owned-views)
for ownership and transport limits.

### Remote pools

```rust
use plasmite::api::{Feed, PoolRef, Read, RemoteClient, Since};
use serde_json::json;

let client = RemoteClient::new("http://127.0.0.1:9700")?;
let pool = client.open_pool(&PoolRef::name("events"), true)?;
let sent = pool.feed(&json!({"kind": "deploy"}), Feed::default().tags(["ops"]))?;
let mut history = pool.fetch(Read::since(Since::Seq(sent.seq)))?;
while let Some(message) = history.next_message()? {
    println!("{}", message.data);
}
```

For HTTPS, save an access key with `plasmite access connect SERVER_URL` or
construct the client with `RemoteClient::with_access_key(SERVER_URL, key)`.
See the [serving guide](docs/record/serving.md) for server setup.

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
`plasmite serve install SERVER` arranges boot startup on macOS, Linux with
systemd, or Windows.
See the [full README](https://github.com/sandover/plasmite) for CLI docs,
cookbook, and language bindings ([Node](https://www.npmjs.com/package/plasmite),
[Python](https://pypi.org/project/plasmite/), [Go](https://github.com/sandover/plasmite/tree/main/bindings/go)).

## License

MIT
