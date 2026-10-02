//! Purpose: Keep one native API client alive for transport benchmarks.
//! Exports: None (newline-delimited benchmark helper).
//! Role: Measure individual local and remote pool calls without CLI startup.
//! Invariants: JSON and Lite3 paths use the same application JSON values.
//! Invariants: Output contains measurements and readback facts, never credentials.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

use plasmite::api::{
    Durability, Lite3DocRef, LocalClient, Pool, PoolApiExt, PoolInfo, PoolRef, RemoteClient,
    RemotePool, lite3,
};
use serde_json::{Value, json};

type ApiResult<T> = Result<T, Box<plasmite::api::Error>>;

enum Connection {
    Local {
        dir: PathBuf,
    },
    Remote {
        server: String,
        key_file: Option<PathBuf>,
        save_connection: bool,
    },
}

#[derive(Clone, Copy)]
enum Encoding {
    Json,
    Lite3,
}

struct Options {
    connection: Connection,
    pool_name: String,
    encoding: Encoding,
}

enum PoolAccess {
    Local(Pool),
    Remote(RemotePool),
}

impl PoolAccess {
    fn open(connection: Connection, pool_name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let pool_ref = PoolRef::name(pool_name);
        let pool = match connection {
            Connection::Local { dir } => {
                let client = LocalClient::new().with_pool_dir(dir);
                Self::Local(client.open_pool(&pool_ref)?)
            }
            Connection::Remote {
                server,
                key_file,
                save_connection,
            } => {
                let client = match key_file {
                    Some(path) => {
                        let access_key = read_access_key(path)?;
                        if save_connection {
                            plasmite::api::access::connect(&server, &access_key)?;
                            RemoteClient::new(server)?
                        } else {
                            RemoteClient::with_access_key(server, &access_key)?
                        }
                    }
                    None => RemoteClient::new(server)?,
                };
                Self::Remote(client.open_pool(&pool_ref)?)
            }
        };
        pool.info()?; // Warm the connection and resolve the pool before timed calls.
        Ok(pool)
    }

    fn info(&self) -> ApiResult<PoolInfo> {
        match self {
            Self::Local(pool) => pool.info(),
            Self::Remote(pool) => pool.info(),
        }
        .map_err(Box::new)
    }

    fn append_json(&mut self, item: &Value) -> ApiResult<u64> {
        match self {
            Self::Local(pool) => pool
                .append_json_now(item, &[], Durability::Fast)
                .map(|message| message.seq),
            Self::Remote(pool) => pool
                .append_json_now(item, &[], Durability::Fast)
                .map(|message| message.seq),
        }
        .map_err(Box::new)
    }

    fn append_lite3(&mut self, payload: &[u8]) -> ApiResult<u64> {
        match self {
            Self::Local(pool) => pool.append_lite3_now(payload, Durability::Fast),
            Self::Remote(pool) => pool.append_lite3_now(payload, Durability::Fast),
        }
        .map_err(Box::new)
    }

    fn get_message(&self, sequence: u64) -> ApiResult<plasmite::api::Message> {
        match self {
            Self::Local(pool) => pool.get_message(sequence),
            Self::Remote(pool) => pool.get_message(sequence),
        }
        .map_err(Box::new)
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let options = parse_args()?;
    let mut pool = PoolAccess::open(options.connection, &options.pool_name)?;

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
                if items.iter().any(|item| !item.is_object()) {
                    return Err("append items must be JSON objects".into());
                }
                let mut latencies = Vec::with_capacity(items.len());
                let mut sequences = Vec::with_capacity(items.len());
                let encoded = match options.encoding {
                    Encoding::Json => None,
                    Encoding::Lite3 => {
                        let mut encoded = Vec::with_capacity(items.len());
                        for item in items {
                            encoded.push(lite3::encode_message(&[], item)?);
                        }
                        Some(encoded)
                    }
                };
                for (index, item) in items.iter().enumerate() {
                    let call_started = Instant::now();
                    let sequence = match &encoded {
                        Some(encoded) => pool.append_lite3(encoded[index].as_slice())?,
                        None => pool.append_json(item)?,
                    };
                    latencies.push(call_started.elapsed().as_nanos());
                    sequences.push(sequence);
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
                if matches!(
                    (&pool, options.encoding),
                    (PoolAccess::Remote(_), Encoding::Lite3)
                ) {
                    ensure_unique_data(expected)?;
                }
                let mut latencies = Vec::with_capacity(sequences.len());
                for (sequence, expected_data) in sequences.iter().zip(expected) {
                    let sequence = sequence
                        .as_u64()
                        .ok_or("sequences must be unsigned integers")?;
                    match options.encoding {
                        Encoding::Json => {
                            let call_started = Instant::now();
                            let message = pool.get_message(sequence)?;
                            latencies.push(call_started.elapsed().as_nanos());
                            if message.seq != sequence {
                                return Err(format!(
                                    "readback returned sequence {} for requested sequence {sequence}",
                                    message.seq
                                )
                                .into());
                            }
                            if message.data != *expected_data {
                                return Err(
                                    format!("readback mismatch for sequence {sequence}").into()
                                );
                            }
                        }
                        Encoding::Lite3 => match &pool {
                            PoolAccess::Local(pool) => {
                                let call_started = Instant::now();
                                let frame = pool.get_lite3(sequence)?;
                                latencies.push(call_started.elapsed().as_nanos());
                                if frame.seq != sequence {
                                    return Err(format!(
                                            "readback returned sequence {} for requested sequence {sequence}",
                                            frame.seq
                                        )
                                        .into());
                                }
                                verify_lite3_data(frame.payload.as_ref(), expected_data, sequence)?;
                            }
                            PoolAccess::Remote(pool) => {
                                let call_started = Instant::now();
                                let payload = pool.get_lite3(sequence)?;
                                latencies.push(call_started.elapsed().as_nanos());
                                verify_lite3_data(&payload, expected_data, sequence)?;
                            }
                        },
                    }
                }
                (latencies, json!({"verified_count": sequences.len()}))
            }
            _ => return Err("op must be info, append, or read".into()),
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

fn parse_args() -> Result<Options, Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next().ok_or("missing server URL")?;
    let (connection, pool_name, encoding) = match first.to_str() {
        Some("--local") => {
            let dir = next_path(&mut args, "pool directory")?;
            let pool_name = next_text(&mut args, "pool name")?;
            (Connection::Local { dir }, pool_name, Encoding::Json)
        }
        Some("--local-lite3") => {
            let dir = next_path(&mut args, "pool directory")?;
            let pool_name = next_text(&mut args, "pool name")?;
            (Connection::Local { dir }, pool_name, Encoding::Lite3)
        }
        Some("--http") => {
            let server = next_text(&mut args, "HTTP server URL")?;
            if !server.starts_with("http://") {
                return Err("--http requires an http:// server URL".into());
            }
            let pool_name = next_text(&mut args, "pool name")?;
            (
                Connection::Remote {
                    server,
                    key_file: None,
                    save_connection: false,
                },
                pool_name,
                Encoding::Json,
            )
        }
        Some("--key") => {
            let server = next_text(&mut args, "server URL")?;
            let pool_name = next_text(&mut args, "pool name")?;
            let key_file = next_path(&mut args, "access-key file")?;
            (
                Connection::Remote {
                    server,
                    key_file: Some(key_file),
                    save_connection: false,
                },
                pool_name,
                Encoding::Json,
            )
        }
        Some("--lite3-key") => {
            let server = next_text(&mut args, "server URL")?;
            let pool_name = next_text(&mut args, "pool name")?;
            let key_file = next_path(&mut args, "access-key file")?;
            (
                Connection::Remote {
                    server,
                    key_file: Some(key_file),
                    save_connection: false,
                },
                pool_name,
                Encoding::Lite3,
            )
        }
        Some("--lite3") => {
            let server = next_text(&mut args, "server URL")?;
            let pool_name = next_text(&mut args, "pool name")?;
            let key_file = next_path(&mut args, "protected access-key file")?;
            (
                Connection::Remote {
                    server,
                    key_file: Some(key_file),
                    save_connection: true,
                },
                pool_name,
                Encoding::Lite3,
            )
        }
        Some(option) if option.starts_with("--") => {
            return Err(format!("unknown option: {option}").into());
        }
        _ => {
            let server = first
                .into_string()
                .map_err(|_| "server URL must be valid UTF-8")?;
            let pool_name = next_text(&mut args, "pool name")?;
            let key_file = next_path(&mut args, "protected access-key file")?;
            (
                Connection::Remote {
                    server,
                    key_file: Some(key_file),
                    save_connection: true,
                },
                pool_name,
                Encoding::Json,
            )
        }
    };
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    Ok(Options {
        connection,
        pool_name,
        encoding,
    })
}

fn read_access_key(path: PathBuf) -> Result<String, Box<dyn std::error::Error>> {
    let key_file = std::fs::read_to_string(path)?;
    let key_file = key_file.trim_start_matches('\u{feff}');
    Ok(serde_json::from_str::<Value>(key_file)
        .ok()
        .and_then(|value| {
            value
                .get("access_key")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| key_file.trim().to_owned()))
}

fn next_path(
    args: &mut impl Iterator<Item = OsString>,
    description: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let argument = args.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("missing {description}"),
        )
    })?;
    Ok(PathBuf::from(argument))
}

fn next_text(
    args: &mut impl Iterator<Item = OsString>,
    description: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let argument = args.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("missing {description}"),
        )
    })?;
    argument
        .into_string()
        .map_err(|_| format!("{description} must be valid UTF-8").into())
}

fn ensure_unique_data(expected: &[Value]) -> Result<(), Box<dyn std::error::Error>> {
    // Remote Lite3 omits sequence headers, so each batch needs distinct expected data.
    let mut seen = HashSet::with_capacity(expected.len());
    for item in expected {
        if !seen.insert(serde_json::to_vec(item)?) {
            return Err("read expected data must be unique within the batch".into());
        }
    }
    Ok(())
}

fn verify_lite3_data(
    payload: &[u8],
    expected: &Value,
    sequence: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let decoded: Value = serde_json::from_str(&Lite3DocRef::new(payload).to_json(false)?)?;
    let data = decoded.get("data").ok_or("Lite3 message has no data")?;
    if data != expected {
        return Err(format!("readback mismatch for sequence {sequence}").into());
    }
    Ok(())
}
