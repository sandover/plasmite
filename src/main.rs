// CLI entry point for v0.0.1 commands with JSON output.
// This file defines the clap surface (commands/flags), does JSON IO, and
// translates CLI inputs into core pool operations and message encoding.
// If you're looking for behavior, search for `run()` and the command handlers.
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};

mod bench;

use clap::{error::ErrorKind as ClapErrorKind, Parser, Subcommand};
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use plasmite::core::error::{to_exit_code, Error, ErrorKind};
use plasmite::core::cursor::{Cursor, CursorResult, FrameRef};
use plasmite::core::lite3::{self, Lite3DocRef};
use plasmite::core::pool::{AppendOptions, Durability, Pool, PoolOptions};

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
        Command::Bench {
            work_dir,
            pool_size,
            payload_bytes,
            messages,
            writers,
            durability,
            format,
        } => {
            let pool_sizes = if pool_size.is_empty() {
                vec![parse_size("1M")?, parse_size("64M")?]
            } else {
                pool_size
                    .iter()
                    .map(|value| parse_size(value))
                    .collect::<Result<Vec<_>, _>>()?
            };

            let payload_sizes = if payload_bytes.is_empty() {
                vec![128usize, 1024usize, 16 * 1024usize]
            } else {
                payload_bytes
                    .iter()
                    .map(|value| parse_usize(value, "payload-bytes"))
                    .collect::<Result<Vec<_>, _>>()?
            };

            let writer_counts = if writers.is_empty() {
                vec![1usize, 2usize, 4usize, 8usize]
            } else {
                writers
                    .iter()
                    .map(|value| parse_usize(value, "writers"))
                    .collect::<Result<Vec<_>, _>>()?
            };

            let durabilities = parse_bench_durabilities(&durability)?;
            let format = bench::BenchFormat::parse(&format)?;
            bench::run_bench(
                bench::BenchArgs {
                    work_dir,
                    pool_sizes,
                    payload_sizes,
                    messages,
                    writers: writer_counts,
                    format,
                    durabilities,
                },
                env!("CARGO_PKG_VERSION"),
            )
        }
        Command::BenchWorker {
            role,
            pool,
            messages,
            payload_bytes,
            durability,
            out_json,
        } => bench::run_worker(bench::WorkerArgs {
            pool_path: pool,
            role: bench::WorkerRole::parse(&role)?,
            messages,
            payload_bytes: payload_bytes as usize,
            out_json,
            durability: parse_durability(&durability)?,
        }),
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
                    results.push(pool_info_json(&name, &info));
                }
                emit_json(json!({ "created": results }));
                Ok(())
            }
            PoolCommand::Info { name } => {
                let path = resolve_poolref(&name, &pool_dir)?;
                let pool = Pool::open(&path)?;
                let info = pool.info()?;
                emit_json(pool_info_json(&name, &info));
                Ok(())
            }
            PoolCommand::Bounds { name } => {
                let path = resolve_poolref(&name, &pool_dir)?;
                let pool = Pool::open(&path)?;
                let bounds = pool.bounds()?;
                emit_json(bounds_with_pool_json(&name, bounds));
                Ok(())
            }
        },
        Command::Poke {
            pool,
            descrip,
            data_json,
            data_file,
            durability,
        } => {
            let path = resolve_poolref(&pool, &pool_dir)?;
            let data = read_data(data_json, data_file)?;
            let payload = lite3::encode_message(&descrip, &data)?;
            let mut pool_handle = Pool::open(&path)?;
            let timestamp_ns = now_ns()?;
            let durability = parse_durability(&durability)?;
            let options = AppendOptions::new(timestamp_ns, durability);
            let seq = pool_handle.append_with_options(payload.as_slice(), options)?;
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
            pretty,
            jsonl,
        } => {
            let path = resolve_poolref(&pool, &pool_dir)?;
            let pool_handle = Pool::open(&path)?;
            let timeout = idle_timeout
                .as_deref()
                .map(parse_duration)
                .transpose()?;
            let pretty = if pretty {
                true
            } else if jsonl {
                false
            } else if follow {
                false
            } else {
                io::stdout().is_terminal()
            };
            peek(
                &pool_handle,
                &pool,
                tail,
                follow,
                timeout,
                pretty,
            )
        }
    }
}

#[derive(Parser)]
#[command(
    name = "plasmite",
    version,
    about = "JSON-first CLI for local plasmite pools",
    long_about = "JSON-first CLI for local plasmite pools.\n\
\n\
Pool references:\n\
  - If the argument contains '/', it's treated as a path.\n\
  - Else if it ends with '.plasmite', it's resolved under the pool dir.\n\
  - Else it's resolved as <POOL_DIR>/<name>.plasmite.\n\
\n\
JSON output is always default. For streams, pretty JSON is used on TTY and\n\
compact JSON is used otherwise.",
)]
struct Cli {
    /// Override the pool directory.
    #[arg(long, help = "Override the pool directory (default: ~/.plasmite/pools)")]
    dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Pool lifecycle and info commands")]
    Pool {
        #[command(subcommand)]
        command: PoolCommand,
    },
    #[command(
        about = "Run a local performance benchmark suite (JSON stdout, table stderr)",
        long_about = "Run a local performance benchmark suite.\n\
\n\
Outputs:\n\
  - JSON to stdout (easy to archive/compare)\n\
  - Table to stderr (human scan)\n\
\n\
Notes:\n\
  - Benchmarks are intended for trend tracking, not lab-grade profiling.\n\
  - Some scenarios spawn child processes to exercise cross-process locking.\n\
  - For baseline numbers, run a release build.\n\
  - Use --durability fast|flush (repeatable) to compare flush impact.\n\
\n\
Examples:\n\
  cargo build --release && ./target/release/plasmite bench\n\
  ./target/release/plasmite bench --format json > bench.json\n\
  plasmite bench --payload-bytes 128 --payload-bytes 1024 --messages 20000\n\
  plasmite bench --durability fast --durability flush\n\
",
    )]
    Bench {
        #[arg(long, help = "Directory for temporary pools/artifacts (default: .scratch/plasmite-bench-<pid>-<ts>)")]
        work_dir: Option<PathBuf>,
        #[arg(long = "pool-size", help = "Repeatable pool size (bytes or K/M/G)")]
        pool_size: Vec<String>,
        #[arg(long = "payload-bytes", help = "Repeatable payload target size (bytes)")]
        payload_bytes: Vec<String>,
        #[arg(long, default_value_t = 20_000, help = "Messages per scenario")]
        messages: u64,
        #[arg(long, help = "Repeatable writer counts for contention scenarios (default: 1,2,4,8)")]
        writers: Vec<String>,
        #[arg(long, help = "Durability mode(s): fast|flush|both (repeatable; default: fast)")]
        durability: Vec<String>,
        #[arg(long, default_value = "both", help = "Output format: json|table|both")]
        format: String,
    },
    #[command(hide = true)]
    BenchWorker {
        #[arg(help = "worker role: writer|follower")]
        role: String,
        #[arg(long = "pool", help = "Pool file path")]
        pool: PathBuf,
        #[arg(long, help = "Message count (writer) or upper bound (follower)")]
        messages: u64,
        #[arg(long = "payload-bytes", help = "Approximate payload size in bytes")]
        payload_bytes: u64,
        #[arg(long, default_value = "fast", help = "Durability mode: fast|flush")]
        durability: String,
        #[arg(long = "out-json", help = "Write worker result JSON to this path")]
        out_json: PathBuf,
    },
    #[command(
        about = "Append a JSON message to a pool",
        long_about = "Append a JSON message to a pool.\n\
\n\
Data input precedence:\n\
  1) --data-json\n\
  2) --data (file path, use @- for stdin)\n\
  3) stdin (only if not a TTY)\n\
\n\
Durability modes:\n\
  - fast (default): best-effort, no explicit flush\n\
  - flush: flush frame + header to storage after append\n\
\n\
Examples:\n\
  plasmite poke demo --descrip ping --data-json '{\"x\":1}'\n\
  plasmite poke demo --data-json '{\"x\":1}' --durability flush\n\
  plasmite poke demo --data @payload.json\n\
  echo '{\"x\":1}' | plasmite poke demo\n\
",
    )]
    Poke {
        #[arg(help = "Pool name or path")]
        pool: String,
        #[arg(long, help = "Repeatable descriptor for meta.descrips")]
        descrip: Vec<String>,
        #[arg(long = "data-json", help = "Inline JSON data", conflicts_with = "data_file")]
        data_json: Option<String>,
        #[arg(long = "data", help = "JSON file path (prefix with @, use @- for stdin)", conflicts_with = "data_json")]
        data_file: Option<String>,
        #[arg(long, default_value = "fast", help = "Durability mode: fast|flush")]
        durability: String,
    },
    #[command(
        about = "Fetch one message by seq",
        long_about = "Fetch one message by seq.\n\
\n\
Example:\n\
  plasmite get demo 42\n\
",
    )]
    Get {
        #[arg(help = "Pool name or path")]
        pool: String,
        #[arg(help = "Sequence number")]
        seq: u64,
    },
    #[command(
        about = "Read or follow messages from a pool",
        long_about = "Read or follow messages from a pool.\n\
\n\
Behavior:\n\
  - With --tail N, prints the last N messages, then exits unless --follow.\n\
  - Without --tail, prints nothing and exits unless --follow.\n\
  - --idle-timeout exits after no activity for the given duration.\n\
\n\
Formatting:\n\
  - Default is pretty JSON on TTY for non-follow reads.\n\
  - Default is JSON Lines for --follow or non-TTY.\n\
  - Override with --pretty or --jsonl.\n\
\n\
Duration suffixes: ms, s, m, h\n\
\n\
Examples:\n\
  plasmite peek demo --tail 10\n\
  plasmite peek demo --follow\n\
  plasmite peek demo --follow --idle-timeout 30s\n\
",
    )]
    Peek {
        #[arg(help = "Pool name or path")]
        pool: String,
        #[arg(long, help = "Emit the last N messages before exiting (or following)")]
        tail: Option<u64>,
        #[arg(long, help = "Block and continue streaming new messages")]
        follow: bool,
        #[arg(long = "idle-timeout", help = "Exit after no activity for the duration (ms|s|m|h)")]
        idle_timeout: Option<String>,
        #[arg(long, help = "Pretty-print JSON output", conflicts_with = "jsonl")]
        pretty: bool,
        #[arg(long, help = "Emit JSON Lines (one object per line)", conflicts_with = "pretty")]
        jsonl: bool,
    },
    #[command(about = "Print version info as JSON")]
    Version,
}

#[derive(Subcommand)]
enum PoolCommand {
    #[command(
        about = "Create one or more pools",
        long_about = "Create one or more pools.\n\
\n\
Examples:\n\
  plasmite pool create demo\n\
  plasmite pool create --size 8M demo-1 demo-2\n\
",
    )]
    Create {
        #[arg(required = true, help = "Pool names (resolved under the pool dir)")]
        names: Vec<String>,
        #[arg(long, help = "Pool size (bytes or K/M/G)")]
        size: Option<String>,
    },
    #[command(
        about = "Show pool metadata and bounds",
        long_about = "Show pool metadata and bounds.\n\
\n\
Example:\n\
  plasmite pool info demo\n\
",
    )]
    Info {
        #[arg(help = "Pool name or path")]
        name: String,
    },
    #[command(
        about = "Show oldest/newest sequence bounds",
        long_about = "Show oldest/newest sequence bounds.\n\
\n\
Example:\n\
  plasmite pool bounds demo\n\
",
    )]
    Bounds {
        #[arg(help = "Pool name or path")]
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
            .with_message("invalid size (expected number like 1M)")
            .with_source(err)
    })?;

    let multiplier = match suffix {
        "" => 1,
        "K" | "k" => 1024,
        "M" | "m" => 1024 * 1024,
        "G" | "g" => 1024 * 1024 * 1024,
        _ => {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("invalid size suffix (use K/M/G)"));
        }
    };

    value
        .checked_mul(multiplier)
        .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("size overflow"))
}

fn parse_usize(input: &str, label: &str) -> Result<usize, Error> {
    input.trim().parse::<usize>().map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message(format!("invalid {label}"))
            .with_source(err)
    })
}

fn parse_bench_durabilities(values: &[String]) -> Result<Vec<Durability>, Error> {
    if values.is_empty() {
        return Ok(vec![Durability::Fast]);
    }

    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.eq_ignore_ascii_case("both") {
            push_durability(&mut out, Durability::Fast);
            push_durability(&mut out, Durability::Flush);
            continue;
        }
        let durability = parse_durability(trimmed)?;
        push_durability(&mut out, durability);
    }

    if out.is_empty() {
        out.push(Durability::Fast);
    }
    Ok(out)
}

fn push_durability(out: &mut Vec<Durability>, durability: Durability) {
    if !out.contains(&durability) {
        out.push(durability);
    }
}

fn parse_durability(input: &str) -> Result<Durability, Error> {
    match input.trim() {
        "fast" => Ok(Durability::Fast),
        "flush" => Ok(Durability::Flush),
        _ => Err(Error::new(ErrorKind::Usage)
            .with_message("invalid --durability (use fast|flush)")),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn parse_size_accepts_bytes_and_kmg() {
        assert_eq!(parse_size("42").unwrap(), 42);
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("2k").unwrap(), 2048);
        assert_eq!(parse_size("3M").unwrap(), 3 * 1024 * 1024);
        assert_eq!(parse_size("4g").unwrap(), 4 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_size_rejects_iec_suffixes() {
        assert!(parse_size("1MiB").is_err());
        assert!(parse_size("2Gi").is_err());
        assert!(parse_size("3KiB").is_err());
    }
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

fn bounds_with_pool_json(pool_ref: &str, bounds: plasmite::core::pool::Bounds) -> Value {
    let mut map = match bounds_json(bounds) {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    map.insert("pool".to_string(), json!(pool_ref));
    Value::Object(map)
}

fn pool_info_json(pool_ref: &str, info: &plasmite::core::pool::PoolInfo) -> Value {
    let mut map = Map::new();
    map.insert("pool".to_string(), json!(pool_ref));
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
        return Err(Error::new(ErrorKind::Usage)
            .with_message("multiple data inputs provided (use only one of --data-json, --data, or stdin)"));
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
        return Err(Error::new(ErrorKind::Usage)
            .with_message("missing data input (use --data-json, --data @FILE, or pipe JSON to stdin)"));
    };

    serde_json::from_str(&json_str)
        .map_err(|err| {
            Error::new(ErrorKind::Usage)
                .with_message("invalid json (expected a JSON value)")
                .with_source(err)
        })
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
    pretty: bool,
) -> Result<(), Error> {
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
            .with_message("invalid duration (expected number like 10s)")
            .with_source(err)
    })?;
    let duration = match suffix {
        "ms" => Duration::from_millis(value),
        "s" => Duration::from_secs(value),
        "m" => Duration::from_secs(value * 60),
        "h" => Duration::from_secs(value * 60 * 60),
        _ => {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("invalid duration suffix (use ms|s|m|h)"));
        }
    };
    Ok(duration)
}
