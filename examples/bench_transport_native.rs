//! Purpose: Keep one native remote API client alive for transport benchmarks.
//! Exports: None (newline-delimited benchmark helper).
//! Role: Measure individual RemotePool append and fetch calls without CLI startup.
//! Invariants: Reads and appends use the same 512-byte application JSON string as MCP.
//! Invariants: Output contains measurements and readback facts, never credentials.

use std::io::{self, BufRead, Write};
use std::time::Instant;

use plasmite::api::{PoolRef, RemoteClient};
use serde_json::{Value, json};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let server = args.next().ok_or("missing server URL")?;
    let pool_name = args.next().ok_or("missing pool name")?;
    let access_key_path = args.next().ok_or("missing protected access-key file")?;
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }

    let key_file = std::fs::read_to_string(access_key_path)?;
    let access_key = serde_json::from_str::<Value>(&key_file)
        .ok()
        .and_then(|value| {
            value
                .get("access_key")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| key_file.trim().to_owned());
    plasmite::api::access::connect(&server, &access_key)?;
    let client = RemoteClient::new(server)?;
    let pool = client.open_pool(&PoolRef::name(pool_name))?;
    pool.info()?; // Warm the connection and resolve the pool before timed calls.

    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line?;
        let request: Value = serde_json::from_str(&line)?;
        let started = Instant::now();
        let (latencies, values) = match request["op"].as_str() {
            Some("info") => {
                let info = pool.info()?;
                (Vec::new(), json!({"file_size": info.file_size}))
            }
            Some("append") => {
                let items = request["items"].as_array().ok_or("append needs items")?;
                let mut latencies = Vec::with_capacity(items.len());
                let mut sequences = Vec::with_capacity(items.len());
                for item in items {
                    if !item.is_object() {
                        return Err("append items must be JSON objects".into());
                    }
                    let call_started = Instant::now();
                    let message =
                        pool.append_json_now(item, &[], plasmite::api::Durability::Fast)?;
                    latencies.push(call_started.elapsed().as_nanos());
                    sequences.push(message.seq);
                }
                (latencies, json!({"sequences": sequences}))
            }
            Some("read") => {
                let sequences = request["sequences"]
                    .as_array()
                    .ok_or("read needs sequences")?;
                let expected = request["expected"]
                    .as_array()
                    .ok_or("read needs expected")?;
                if sequences.len() != expected.len() {
                    return Err("sequence and expected arrays differ in length".into());
                }
                let mut latencies = Vec::with_capacity(sequences.len());
                for (sequence, expected_data) in sequences.iter().zip(expected) {
                    let sequence = sequence
                        .as_u64()
                        .ok_or("sequences must be unsigned integers")?;
                    let call_started = Instant::now();
                    let message = pool.get_message(sequence)?;
                    latencies.push(call_started.elapsed().as_nanos());
                    if message.data != *expected_data {
                        return Err(format!("readback mismatch for sequence {sequence}").into());
                    }
                }
                (latencies, json!({"verified_count": sequences.len()}))
            }
            _ => return Err("op must be append or read".into()),
        };

        let response = json!({
            "elapsed_ns": started.elapsed().as_nanos(),
            "latencies_ns": latencies,
            "result": values,
        });
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}
