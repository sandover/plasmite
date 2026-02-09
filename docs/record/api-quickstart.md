# API Quickstart (Rust)

This guide shows the basic flow using the Rust public API:

- create a client
- create/open a pool
- append messages
- tail messages
- handle errors by kind

## Create a client and pool

```rust
use plasmite::api::{LocalClient, PoolRef, PoolOptions};

let client = LocalClient::new();
let pool_ref = PoolRef::name("events");
let _info = client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
let mut pool = client.open_pool(&pool_ref)?;
```

## Append a message

```rust
use plasmite::api::{PoolApiExt, Durability};
use serde_json::json;

let data = json!({"msg": "hello"});
let tags = vec!["greeting".to_string()];
let message = pool.append_json_now(&data, &tags, Durability::Fast)?;
println!("seq={}", message.seq);
```

## Get a message by seq

```rust
let fetched = pool.get_message(1)?;
println!("{}", fetched.time);
```

## Inspect pool metrics

```rust
let info = pool.info()?;
if let Some(metrics) = info.metrics {
    println!("messages={}", metrics.message_count);
    println!(
        "used_percent={:.2}",
        (metrics.utilization.used_percent_hundredths as f64) / 100.0
    );
}
```

## Tail messages

```rust
use plasmite::api::TailOptions;

let mut tail = pool.tail(TailOptions::default());
while let Some(message) = tail.next_message()? {
    println!("{}", message.seq);
}
```

## Error handling

```rust
use plasmite::api::ErrorKind;

match pool.get_message(9999) {
    Ok(_) => {}
    Err(err) if err.kind() == ErrorKind::NotFound => {
        eprintln!("message not found");
    }
    Err(err) => return Err(err),
}
```

## Notes

- The normative API contract is in `spec/api/v0/SPEC.md`.
- Pool name resolution matches the CLI rules from `spec/v0/SPEC.md`.
