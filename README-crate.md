# plasmite

Plasmite is a pipe that remembers. Programs send and receive JSON messages through a pool: a bounded ring buffer in one file on disk. The pool keeps recent messages after writers exit, and when it fills, new messages overwrite the oldest ones. Local pools need no server or broker.

Three verbs cover almost everything:

- `feed` writes a message.
- `follow` waits for new messages.
- `fetch` reads what the pool holds and stops.

This crate is the Rust library and the `plasmite` CLI. Python, Node, Go and C libraries share the same model.

## Quick start

```rust
use plasmite::api::{Feed, LocalClient, PoolRef, Read};
use serde_json::json;

fn main() -> Result<(), plasmite::api::Error> {
    let client = LocalClient::new();
    let mut pool = client.open_pool(&PoolRef::name("builds"), true)?;
    pool.feed(&json!({"step": "test", "status": "ok"}), Feed::new().tags(["ci"]))?;

    let mut history = pool.fetch(Read::last(10).tags(["ci"]))?;
    while let Some(message) = history.next_message()? {
        println!("{} {}", message.seq, message.data()?);
    }
    Ok(())
}
```

`LocalClient::new()` keeps pools in `~/.plasmite/pools`; `with_pool_dir` picks another directory. Passing `true` to `open_pool` creates a missing pool at the default size of 1 MiB.

Each `Message` has a sequence number in `seq` and a Unix nanosecond timestamp in `time`. Call `tags()` for its labels and `data()` for the JSON object. `sender()` returns the access key name for a write made through a server with that key.

## Choose where a read starts

`follow` and `fetch` take the same `Read` options:

- `Read::new()`: `follow` waits for the next new message; `fetch` reads everything the pool holds.
- `Read::since(Since::Oldest)`, `Since::Seq(n)`, `Since::Ago(duration)`, or `Since::At(unix_nanos)`: start at that position, inclusive.
- `Read::last(n)`: start `n` messages back. It counts messages before the tag filter.
- `.tags(["ci"])`: keep only messages that carry every listed tag.
- `.cancel(flag)`: stop a blocking `follow` when the caller sets an `Arc<AtomicBool>` to true. Drop the reader when you finish using it.

`fetch` stops at the newest message present when it opens. A feed with default options returns after committing the message to the pool. Use `Feed::new().durability(Durability::Flush)` to flush the write before returning.

## Resume after a restart

Save the `seq` of each message after you handle it, and start the next run from the one after it. If the pool has overwritten messages you never read, you get a `RetentionGap` error that names the first missing sequence and the `next_seq` where you can continue:

```rust
use plasmite::api::{Error, ErrorKind, LocalClient, PoolRef, Read, Since};

fn main() -> Result<(), Error> {
    let pool = LocalClient::new().open_pool(&PoolRef::name("builds"), true)?;
    let mut next = 1; // the seq after the last one you handled, from your own storage
    loop {
        let result = pool.follow(Read::since(Since::Seq(next))).and_then(|mut reader| {
            while let Some(message) = reader.next_message()? {
                println!("{} {}", message.seq, message.data()?);
                next = message.seq + 1; // save this after handling the message
            }
            Ok(())
        });
        match result {
            Err(err) if err.kind() == ErrorKind::RetentionGap => {
                eprintln!("missed messages from {:?}", err.missing());
                next = err.next_seq().unwrap();
            }
            other => return other,
        }
    }
}
```

## Manage pools

```rust
use plasmite::api::{LocalClient, PoolDescription, PoolOptions, PoolRef};

fn main() -> Result<(), plasmite::api::Error> {
    let client = LocalClient::new();
    let sensors = PoolRef::name("sensors");
    let description = PoolDescription {
        about: Some("Kitchen temperature readings".into()),
        example: Some(serde_json::json!({"temp_c": 21.4})),
    };
    let options = PoolOptions::new(64 * 1024 * 1024).with_description(description);
    let mut pool = client.create_pool(&sensors, options)?;
    println!("bounds: {:?}", pool.info()?.bounds);
    println!("description: {:?}", pool.info()?.description);
    println!("{} pools", client.list_pools()?.len());
    pool.set_description(PoolDescription::default())?;
    drop(pool);
    client.delete_pool(&sensors)?;
    Ok(())
}
```

`set_description()` replaces both fields. An empty `PoolDescription` clears them.
`PoolRef::path` names a pool file directly. `client.validate_pool(&reference)` checks a pool for corruption.

## Remote pools

`RemoteClient` reads and writes pools that `plasmite serve` shares, with the same `Feed` and `Read` options:

```rust
use plasmite::api::{Feed, PoolRef, Read, RemoteClient};
use serde_json::json;

fn main() -> Result<(), plasmite::api::Error> {
    let client = RemoteClient::new("https://pools.example.net:9743")?;
    let pool = client.open_pool(&PoolRef::name("sensors"), true)?;
    pool.feed(&json!({"temp_c": 21.4}), Feed::new())?;
    let mut history = pool.fetch(Read::last(10))?;
    while let Some(message) = history.next_message()? {
        println!("{}", message.data()?);
    }
    Ok(())
}
```

`RemoteClient::new` uses the access key that `plasmite access connect` saved for that server; `RemoteClient::with_access_key(url, key)` takes one directly. The [serving guide](https://github.com/sandover/plasmite/blob/main/docs/record/serving.md) covers server setup. `UdpClient` sends and follows over UDP on a trusted network.

## Keep, decode, or forward a message

`feed`, `follow`, and `fetch` use one owned `Message`. You can read its `seq`, `time`, `tags()`, or `sender()` without decoding the application object. `data()` decodes that object on first access and caches it. Use `decode()` when you only need fields named in a Rust type.

```rust
use plasmite::api::{Feed, LocalClient, PoolRef, Read};

#[derive(serde::Deserialize)]
struct Reading {
    temp_c: f64,
}

fn main() -> Result<(), plasmite::api::Error> {
    let client = LocalClient::new();
    let pool = client.open_pool(&PoolRef::name("sensors"), true)?;
    let mut archive = client.open_pool(&PoolRef::name("archive"), true)?;
    for message in pool.fetch(Read::last(10))? {
        let message = message?;
        println!("{}", message.decode::<Reading>()?.temp_c);
        archive.feed(&message, Feed::new())?;
    }
    Ok(())
}
```

A message stays valid after you close its pool or reader. `raw()` borrows the stored Lite3 bytes. `FeedInput::Json` and `FeedInput::Lite3` let you pass explicitly encoded bytes to `feed`. Forwarding a `Message` retains its data and tags and gives it a new sequence and time. A shared server uses the destination key to set the sender. See the [2.0 contract](https://github.com/sandover/plasmite/blob/main/spec/v2/SPEC.md).

## CLI

`cargo install plasmite` installs the `plasmite` CLI and its short alias `pls`:

```bash
plasmite follow builds                                   # terminal 1: waits
plasmite feed builds '{"step": "test", "status": "ok"}'  # terminal 2
```

The [README](https://github.com/sandover/plasmite#readme) covers the CLI, sharing pools across machines, and other install channels. The [Cookbook](https://github.com/sandover/plasmite/blob/main/docs/cookbook.md) has task-by-task recipes.

## License

MIT
