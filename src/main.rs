// CLI entry point for v0.0.1 commands with JSON output.
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};

use clap::{error::ErrorKind as ClapErrorKind, Parser, Subcommand};
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use plasmite::core::error::{to_exit_code, Error, ErrorKind};
use plasmite::core::cursor::{Cursor, CursorResult, FrameRef};
use plasmite::core::lite3::{self, Lite3DocRef};
use plasmite::core::pool::{Pool, PoolOptions};

fn main() {
    let exit_code = match run() {
        Ok(()) => 0,
        Err(err) => {
            emit_error(&err);
            to_exit_code(err.kind())
        }
    };
    std::process::exit(exit_code);
}

fn run() -> Result<(), Error> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            match err.kind() {
                ClapErrorKind::DisplayHelp | ClapErrorKind::DisplayVersion => {
                    err.print().map_err(|io_err| {
                        Error::new(ErrorKind::Io)
                            .with_message("failed to write help")
                            .with_source(io_err)
                    })?;
                    return Ok(());
                }
                _ => {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message(err.to_string()));
                }
            }
        }
    };

    let pool_dir = cli.dir.unwrap_or_else(default_pool_dir);

    match cli.command {
        Command::Version => {
            let output = json!({
                "name": "plasmite",
                "version": env!("CARGO_PKG_VERSION"),
            });
            emit_json(output);
            Ok(())
        }
        Command::Pool { command } => match command {
            PoolCommand::Create { names, size } => {
                let size = size
                    .as_deref()
                    .map(parse_size)
                    .transpose()?
                    .unwrap_or(DEFAULT_POOL_SIZE);
                ensure_pool_dir(&pool_dir)?;
                let mut results = Vec::new();
                for name in names {
                    let path = resolve_poolref(&name, &pool_dir)?;
                    if path.exists() {
                        return Err(Error::new(ErrorKind::AlreadyExists)
                            .with_message("pool already exists")
                            .with_path(&path));
                    }
                    let pool = Pool::create(&path, PoolOptions::new(size))?;
                    let info = pool.info()?;
                    results.push(pool_info_json(&info));
                }
                emit_json(Value::Array(results));
                Ok(())
            }
            PoolCommand::Info { name } => {
                let path = resolve_poolref(&name, &pool_dir)?;
                let pool = Pool::open(&path)?;
                let info = pool.info()?;
                emit_json(pool_info_json(&info));
                Ok(())
            }
            PoolCommand::Bounds { name } => {
                let path = resolve_poolref(&name, &pool_dir)?;
                let pool = Pool::open(&path)?;
                let bounds = pool.bounds()?;
                emit_json(bounds_json(bounds));
                Ok(())
            }
        },
        Command::Poke {
            pool,
            descrip,
            data_json,
            data_file,
        } => {
            let path = resolve_poolref(&pool, &pool_dir)?;
            let data = read_data(data_json, data_file)?;
            let payload = lite3::encode_message(&descrip, &data)?;
            let mut pool_handle = Pool::open(&path)?;
            let timestamp_ns = now_ns()?;
            let seq = pool_handle.append_with_timestamp(payload.as_slice(), timestamp_ns)?;
            emit_json(message_json(&pool, seq, timestamp_ns, &descrip, &data)?);
            Ok(())
        }
        Command::Get { pool, seq } => {
            let path = resolve_poolref(&pool, &pool_dir)?;
            let pool_handle = Pool::open(&path)?;
            let frame = pool_handle.get(seq)?;
            emit_json(message_from_frame(&pool, &frame)?);
            Ok(())
        }
        Command::Peek {
            pool,
            tail,
            follow,
            idle_timeout,
        } => {
            let path = resolve_poolref(&pool, &pool_dir)?;
            let pool_handle = Pool::open(&path)?;
            let timeout = idle_timeout
                .as_deref()
                .map(parse_duration)
                .transpose()?;
            peek(
                &pool_handle,
                &pool,
                tail,
                follow,
                timeout,
            )
        }
    }
}

#[derive(Parser)]
#[command(name = "plasmite", version, about = "Plasmite CLI")]
struct Cli {
    /// Override the pool directory.
    #[arg(long)]
    dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Pool {
        #[command(subcommand)]
        command: PoolCommand,
    },
    Poke {
        pool: String,
        #[arg(long)]
        descrip: Vec<String>,
        #[arg(long = "data-json")]
        data_json: Option<String>,
        #[arg(long = "data")]
        data_file: Option<String>,
    },
    Get {
        pool: String,
        seq: u64,
    },
    Peek {
        pool: String,
        #[arg(long)]
        tail: Option<u64>,
        #[arg(long)]
        follow: bool,
        #[arg(long = "idle-timeout")]
        idle_timeout: Option<String>,
    },
    Version,
}

#[derive(Subcommand)]
enum PoolCommand {
    Create {
        #[arg(required = true)]
        names: Vec<String>,
        #[arg(long)]
        size: Option<String>,
    },
    Info {
        name: String,
    },
    Bounds {
        name: String,
    },
}

fn default_pool_dir() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".plasmite").join("pools")
}

fn resolve_poolref(input: &str, pool_dir: &Path) -> Result<PathBuf, Error> {
    if input.contains('/') {
        return Ok(PathBuf::from(input));
    }
    if input.ends_with(".plasmite") {
        return Ok(pool_dir.join(input));
    }
    Ok(pool_dir.join(format!("{input}.plasmite")))
}

const DEFAULT_POOL_SIZE: u64 = 1024 * 1024;

fn ensure_pool_dir(dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dir)
        .map_err(|err| Error::new(ErrorKind::Io).with_path(dir).with_source(err))
}

fn parse_size(input: &str) -> Result<u64, Error> {
    let trimmed = input.trim();
    let split = trimmed
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| trimmed.len());
    let digits = trimmed[..split].trim();
    let suffix = trimmed[split..].trim();

    let value: u64 = digits.trim().parse().map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid size")
            .with_source(err)
    })?;

    let multiplier = match suffix {
        "" => 1,
        "K" | "k" | "KiB" | "Ki" => 1024,
        "M" | "m" | "MiB" | "Mi" => 1024 * 1024,
        "G" | "g" | "GiB" | "Gi" => 1024 * 1024 * 1024,
        _ => {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("invalid size suffix"));
        }
    };

    value
        .checked_mul(multiplier)
        .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("size overflow"))
}

fn bounds_json(bounds: plasmite::core::pool::Bounds) -> Value {
    let mut map = Map::new();
    if let Some(oldest) = bounds.oldest_seq {
        map.insert("oldest".to_string(), json!(oldest));
    }
    if let Some(newest) = bounds.newest_seq {
        map.insert("newest".to_string(), json!(newest));
    }
    Value::Object(map)
}

fn pool_info_json(info: &plasmite::core::pool::PoolInfo) -> Value {
    let mut map = Map::new();
    map.insert("path".to_string(), json!(info.path.display().to_string()));
    map.insert("file_size".to_string(), json!(info.file_size));
    map.insert("ring_offset".to_string(), json!(info.ring_offset));
    map.insert("ring_size".to_string(), json!(info.ring_size));
    map.insert("bounds".to_string(), bounds_json(info.bounds));
    Value::Object(map)
}

fn emit_json(value: serde_json::Value) {
    let json = if io::stdout().is_terminal() {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string());
    println!("{json}");
}

fn emit_message(value: serde_json::Value, pretty: bool) {
    let json = if pretty {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string());
    println!("{json}");
}

fn emit_error(err: &Error) {
    let value = json!({
        "error": {
            "kind": format!("{:?}", err.kind()),
            "message": err.to_string(),
        }
    });
    let json = if io::stderr().is_terminal() {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string());
    eprintln!("{json}");
}

fn read_data(data_json: Option<String>, data_file: Option<String>) -> Result<Value, Error> {
    let provided = data_json.is_some() as u8 + data_file.is_some() as u8;
    if provided > 1 {
        return Err(Error::new(ErrorKind::Usage).with_message("multiple data inputs provided"));
    }

    let json_str = if let Some(data) = data_json {
        data
    } else if let Some(data) = data_file {
        let path = data.strip_prefix('@').unwrap_or(&data);
        if path == "-" {
            read_stdin()?
        } else {
            std::fs::read_to_string(path)
                .map_err(|err| Error::new(ErrorKind::Io).with_message("failed to read data file").with_source(err))?
        }
    } else if !io::stdin().is_terminal() {
        read_stdin()?
    } else {
        return Err(Error::new(ErrorKind::Usage).with_message("missing data input"));
    };

    serde_json::from_str(&json_str)
        .map_err(|err| Error::new(ErrorKind::Usage).with_message("invalid json").with_source(err))
}

fn read_stdin() -> Result<String, Error> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .map_err(|err| Error::new(ErrorKind::Io).with_message("failed to read stdin").with_source(err))?;
    Ok(buf)
}

fn now_ns() -> Result<u64, Error> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| Error::new(ErrorKind::Internal).with_message("time went backwards").with_source(err))?;
    Ok(duration.as_nanos() as u64)
}

fn format_ts(timestamp_ns: u64) -> Result<String, Error> {
    use time::format_description::well_known::Rfc3339;
    let ts = time::OffsetDateTime::from_unix_timestamp_nanos(timestamp_ns as i128)
        .map_err(|err| Error::new(ErrorKind::Internal).with_message("invalid timestamp").with_source(err))?;
    ts.format(&Rfc3339)
        .map_err(|err| Error::new(ErrorKind::Internal).with_message("timestamp format failed").with_source(err))
}

fn message_json(
    pool_ref: &str,
    seq: u64,
    timestamp_ns: u64,
    descrips: &[String],
    data: &Value,
) -> Result<Value, Error> {
    let meta = json!({ "descrips": descrips });
    Ok(json!({
        "pool": pool_ref,
        "seq": seq,
        "ts": format_ts(timestamp_ns)?,
        "meta": meta,
        "data": data,
    }))
}

fn message_from_frame(pool_ref: &str, frame: &FrameRef<'_>) -> Result<Value, Error> {
    let (meta, data) = decode_payload(frame.payload)?;
    Ok(json!({
        "pool": pool_ref,
        "seq": frame.seq,
        "ts": format_ts(frame.timestamp_ns)?,
        "meta": meta,
        "data": data,
    }))
}

fn decode_payload(payload: &[u8]) -> Result<(Value, Value), Error> {
    let json_str = Lite3DocRef::new(payload).to_json(false)?;
    let value: Value = serde_json::from_str(&json_str)
        .map_err(|err| Error::new(ErrorKind::Corrupt).with_message("invalid payload json").with_source(err))?;
    let obj = value.as_object().ok_or_else(|| {
        Error::new(ErrorKind::Corrupt).with_message("payload is not object")
    })?;
    let meta = obj
        .get("meta")
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing meta"))?;
    let data = obj
        .get("data")
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing data"))?;
    Ok((meta, data))
}

fn peek(
    pool: &Pool,
    pool_ref: &str,
    tail: Option<u64>,
    follow: bool,
    idle_timeout: Option<Duration>,
) -> Result<(), Error> {
    let pretty = io::stdout().is_terminal();
    let mut cursor = Cursor::new();
    let mut header = pool.header_from_mmap()?;
    let mut emit = VecDeque::new();

    if let Some(tail_count) = tail {
        cursor.seek_to(header.tail_off as usize);
        loop {
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    emit.push_back(message_from_frame(pool_ref, &frame)?);
                    while emit.len() > tail_count as usize {
                        emit.pop_front();
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
        for value in emit.drain(..) {
            emit_message(value, pretty);
        }
    } else if !follow {
        return Ok(());
    }

    if !follow {
        return Ok(());
    }

    if tail.is_none() {
        cursor.seek_to(header.head_off as usize);
    }

    let mut backoff = Duration::from_millis(1);
    let max_backoff = Duration::from_millis(50);
    let mut last_activity = SystemTime::now();

    loop {
        match cursor.next(pool)? {
            CursorResult::Message(frame) => {
                emit_message(message_from_frame(pool_ref, &frame)?, pretty);
                backoff = Duration::from_millis(1);
                last_activity = SystemTime::now();
            }
            CursorResult::WouldBlock => {
                if let Some(timeout) = idle_timeout {
                    if SystemTime::now()
                        .duration_since(last_activity)
                        .unwrap_or_default()
                        >= timeout
                    {
                        break;
                    }
                }
                std::thread::sleep(backoff);
                backoff = std::cmp::min(backoff * 2, max_backoff);
            }
            CursorResult::FellBehind => {
                header = pool.header_from_mmap()?;
                if tail.is_some() {
                    cursor.seek_to(header.tail_off as usize);
                } else {
                    cursor.seek_to(header.head_off as usize);
                }
            }
        }
    }

    Ok(())
}

fn parse_duration(input: &str) -> Result<Duration, Error> {
    let trimmed = input.trim();
    let split = trimmed
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| trimmed.len());
    let digits = trimmed[..split].trim();
    let suffix = trimmed[split..].trim();
    let value: u64 = digits.parse().map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid duration")
            .with_source(err)
    })?;
    let duration = match suffix {
        "ms" => Duration::from_millis(value),
        "s" => Duration::from_secs(value),
        "m" => Duration::from_secs(value * 60),
        "h" => Duration::from_secs(value * 60 * 60),
        _ => {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("invalid duration suffix"));
        }
    };
    Ok(duration)
}
