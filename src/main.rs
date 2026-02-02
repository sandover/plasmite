//! Purpose: `plasmite` CLI entry point and v0.0.1 command dispatch.
//! Role: Binary crate root; parses args, runs commands, emits JSON on stdout.
//! Invariants: Successful command output is JSON on stdout; errors are JSON on stderr.
//! Invariants: Process exit code is derived from `core::error::to_exit_code`.
//! Invariants: All pool mutations go through `core::pool` (locks + mmap safety).
#![allow(clippy::result_large_err)]
use std::ffi::OsString;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum, error::ErrorKind as ClapErrorKind};
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::error::Error as StdError;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use plasmite::core::cursor::{Cursor, CursorResult, FrameRef};
use plasmite::core::error::{Error, ErrorKind, to_exit_code};
use plasmite::core::lite3::{self, Lite3DocRef};
use plasmite::core::pool::{AppendOptions, Durability, Pool, PoolOptions};
use plasmite::notice::{Notice, notice_json};

fn main() {
    let exit_code = match run() {
        Ok(()) => 0,
        Err((err, color_mode)) => {
            emit_error(&err, color_mode);
            to_exit_code(err.kind())
        }
    };
    std::process::exit(exit_code);
}

fn run() -> Result<(), (Error, ColorMode)> {
    let cli = match Cli::try_parse_from(normalize_args(std::env::args_os())) {
        Ok(cli) => cli,
        Err(err) => match err.kind() {
            ClapErrorKind::DisplayHelp | ClapErrorKind::DisplayVersion => {
                err.print().map_err(|io_err| {
                    (
                        Error::new(ErrorKind::Io)
                            .with_message("failed to write help")
                            .with_source(io_err),
                        ColorMode::Auto,
                    )
                })?;
                return Ok(());
            }
            _ => {
                let message = clap_error_summary(&err);
                let hint = clap_error_hint(&err);
                return Err((
                    Error::new(ErrorKind::Usage)
                        .with_message(message)
                        .with_hint(hint),
                    ColorMode::Auto,
                ));
            }
        },
    };

    let pool_dir = cli.dir.unwrap_or_else(default_pool_dir);
    let color_mode = cli.color;

    let result = (|| match cli.command {
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
                            .with_path(&path)
                            .with_hint(
                                "Choose a different name or remove the existing pool file.",
                            ));
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
                let pool =
                    Pool::open(&path).map_err(|err| add_missing_pool_hint(err, &name, &name))?;
                let info = pool.info()?;
                emit_json(pool_info_json(&name, &info));
                Ok(())
            }
            PoolCommand::Delete { name } => {
                let path = resolve_poolref(&name, &pool_dir)?;
                std::fs::remove_file(&path).map_err(|err| {
                    if err.kind() == std::io::ErrorKind::NotFound {
                        Error::new(ErrorKind::NotFound)
                            .with_message("pool not found")
                            .with_path(&path)
                            .with_hint("Create the pool first or check --dir.")
                    } else {
                        Error::new(ErrorKind::Io)
                            .with_message("failed to delete pool")
                            .with_path(&path)
                            .with_source(err)
                    }
                })?;
                emit_json(json!({
                    "deleted": {
                        "pool": name,
                        "path": path.display().to_string(),
                    }
                }));
                Ok(())
            }
        },
        Command::Poke {
            pool,
            descrip,
            data,
            file,
            durability,
            create,
            create_size,
        } => {
            let path = resolve_poolref(&pool, &pool_dir)?;
            if create_size.is_some() && !create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("--create-size requires --create")
                    .with_hint("Add --create or remove --create-size."));
            }
            let mut pool_handle = match Pool::open(&path) {
                Ok(pool) => pool,
                Err(err) if create && err.kind() == ErrorKind::NotFound => {
                    ensure_pool_dir(&pool_dir)?;
                    let size = create_size
                        .as_deref()
                        .map(parse_size)
                        .transpose()?
                        .unwrap_or(DEFAULT_POOL_SIZE);
                    Pool::create(&path, PoolOptions::new(size))?
                }
                Err(err) => {
                    return Err(add_missing_pool_hint(err, &pool, &pool));
                }
            };
            let durability = parse_durability(&durability)?;
            if data.is_some() && file.is_some() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("multiple data inputs provided")
                    .with_hint("Use only one of DATA, --file, or stdin."));
            }

            if data.is_some() || file.is_some() || io::stdin().is_terminal() {
                let data = read_data_single(data, file)?;
                let payload = lite3::encode_message(&descrip, &data)?;
                let timestamp_ns = now_ns()?;
                let options = AppendOptions::new(timestamp_ns, durability);
                let seq = pool_handle.append_with_options(payload.as_slice(), options)?;
                emit_json(message_json(seq, timestamp_ns, &descrip, &data)?);
            } else {
                let count = read_json_stream(io::stdin().lock(), |data| {
                    let payload = lite3::encode_message(&descrip, &data)?;
                    let timestamp_ns = now_ns()?;
                    let options = AppendOptions::new(timestamp_ns, durability);
                    let seq = pool_handle.append_with_options(payload.as_slice(), options)?;
                    emit_message(message_json(seq, timestamp_ns, &descrip, &data)?, false);
                    Ok(())
                })?;
                if count == 0 {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("missing data input")
                        .with_hint("Provide JSON via DATA, --file, or pipe JSON to stdin."));
                }
            }
            Ok(())
        }
        Command::Get { pool, seq } => {
            let path = resolve_poolref(&pool, &pool_dir)?;
            let pool_handle =
                Pool::open(&path).map_err(|err| add_missing_pool_hint(err, &pool, &pool))?;
            let frame = pool_handle
                .get(seq)
                .map_err(|err| add_missing_seq_hint(err, &pool))?;
            emit_json(message_from_frame(&frame)?);
            Ok(())
        }
        Command::Peek {
            pool,
            jsonl,
            tail,
            quiet_drops,
            format,
        } => {
            let path = resolve_poolref(&pool, &pool_dir)?;
            let pool_handle =
                Pool::open(&path).map_err(|err| add_missing_pool_hint(err, &pool, &pool))?;
            if jsonl && format.is_some() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("conflicting output options")
                    .with_hint("Use --format jsonl (or --jsonl), but not both."));
            }
            let format = format.unwrap_or(if jsonl {
                PeekFormat::Jsonl
            } else {
                PeekFormat::Pretty
            });
            let pretty = matches!(format, PeekFormat::Pretty);
            peek(
                &pool_handle,
                &pool,
                &path,
                tail,
                pretty,
                quiet_drops,
                color_mode,
            )
        }
    })();

    result
        .map_err(add_corrupt_hint)
        .map_err(add_io_hint)
        .map_err(|err| (err, color_mode))
}

fn normalize_args<I>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    args.into_iter()
        .map(|arg| {
            let replacement = arg.to_str().and_then(|value| match value {
                "---help" => Some("--help"),
                "---version" => Some("--version"),
                _ => None,
            });
            replacement.map(OsString::from).unwrap_or_else(|| arg)
        })
        .collect()
}

#[derive(Parser)]
#[command(
    name = "plasmite",
    version,
    about = "Local, JSON-first message pools",
    help_template = r#"{about-with-newline}
{before-help}USAGE
  {usage}

COMMANDS
{subcommands}

OPTIONS
{options}

{after-help}
"#,
    long_about = None,
    before_help = r#"Plasmite stores immutable JSON messages in a single-file, mmap-backed ring buffer.

CORE COMMANDS
  pool create   Create a pool file
  poke          Append JSON messages
  peek          Watch messages
  get           Fetch a message by sequence
"#,
    after_help = r#"EXAMPLES
  $ plasmite pool create demo
  $ plasmite poke demo --descrip ping '{"x":1}'
  $ plasmite peek demo
  $ plasmite peek demo --tail 10

LEARN MORE
  $ plasmite <command> --help
  https://github.com/sandover/plasmite"#,
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
struct Cli {
    /// Override the pool directory.
    #[arg(
        long,
        help = "Override the pool directory (default: ~/.plasmite/pools)"
    )]
    dir: Option<PathBuf>,
    #[arg(
        long,
        default_value = "auto",
        value_enum,
        help = "Colorize stderr diagnostics: auto|always|never"
    )]
    color: ColorMode,

    #[command(subcommand)]
    command: Command,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum PeekFormat {
    Pretty,
    Jsonl,
}

impl ColorMode {
    fn use_color(self, is_tty: bool) -> bool {
        match self {
            ColorMode::Auto => is_tty,
            ColorMode::Always => true,
            ColorMode::Never => false,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    #[command(
        arg_required_else_help = true,
        about = "Manage pool files",
        long_about = r#"Create and inspect pool files (single-file, mmap-backed rings on disk).

Use `pool create` first, then use `poke` / `peek` / `get` to write and read messages."#,
        after_help = r#"EXAMPLES
  $ plasmite pool create demo
  $ plasmite pool create --size 8M demo-1 demo-2
  $ plasmite pool info demo
  $ plasmite pool delete demo

NOTES
  - Pools live under the pool directory (default: ~/.plasmite/pools); override with `--dir`."#
    )]
    Pool {
        #[command(subcommand)]
        command: PoolCommand,
    },
    #[command(
        about = "Append a message to a pool",
        long_about = r#"Append JSON messages to a pool.

Accepts JSON from a single inline value, a file, or piped stdin."#,
        after_help = r#"EXAMPLES
  $ plasmite poke demo --descrip ping '{"x":1}'
  $ plasmite poke demo --file payload.json
  $ echo '{"x":1}' | plasmite poke demo
  $ printf '%s\n' '{"x":1}' '{"x":2}' | plasmite poke demo

NOTES
  - `--durability flush` trades throughput for stronger “written to storage” assurance."#
    )]
    Poke {
        #[arg(help = "Pool name or path")]
        pool: String,
        #[arg(help = "Inline JSON value")]
        data: Option<String>,
        #[arg(long, help = "Repeatable tag/descriptor for the message")]
        descrip: Vec<String>,
        #[arg(
            long = "file",
            help = "JSON file path (use - for stdin)",
            conflicts_with = "data"
        )]
        file: Option<String>,
        #[arg(long, default_value = "fast", help = "Durability mode: fast|flush")]
        durability: String,
        #[arg(long, help = "Create the pool if it is missing")]
        create: bool,
        #[arg(
            long = "create-size",
            help = "Pool size when creating (bytes or K/M/G)"
        )]
        create_size: Option<String>,
    },
    #[command(
        about = "Fetch one message by sequence number",
        long_about = r#"Fetch a committed message by sequence number and print it as JSON."#,
        after_help = r#"EXAMPLE
  $ plasmite get demo 42"#
    )]
    Get {
        #[arg(help = "Pool name or path")]
        pool: String,
        #[arg(help = "Sequence number")]
        seq: u64,
    },
    #[command(
        about = "Watch messages from a pool",
        long_about = r#"Watch a pool and print messages to stdout.

By default, `peek` waits for new messages and prints them as they arrive.
Use `--tail N` to print the last N messages first, then keep watching."#,
        after_help = r#"EXAMPLES
  $ plasmite peek demo
  $ plasmite peek demo --tail 10
  $ plasmite peek demo --jsonl | jq -c '.data'

NOTES
  - Default output is pretty-printed JSON per message.
  - Use `--format jsonl` for compact, one-object-per-line output (recommended for pipes/scripts).
  - `--jsonl` is a compatibility alias for `--format jsonl`.
  - Drop notices are emitted on stderr when the reader falls behind; use --quiet-drops to suppress."#
    )]
    Peek {
        #[arg(help = "Pool name or path")]
        pool: String,
        #[arg(
            long = "tail",
            short = 'n',
            default_value_t = 0,
            help = "Print the last N messages first, then keep watching"
        )]
        tail: u64,
        #[arg(long, help = "Emit JSON Lines (one object per line)")]
        jsonl: bool,
        #[arg(
            long,
            value_enum,
            help = "Output format: pretty|jsonl (use --jsonl as alias for jsonl)"
        )]
        format: Option<PeekFormat>,
        #[arg(long = "quiet-drops", help = "Suppress drop notices on stderr")]
        quiet_drops: bool,
    },
    #[command(
        about = "Print version info as JSON",
        long_about = r#"Emit version info as JSON (stable, machine-readable)."#,
        after_help = r#"EXAMPLE
  $ plasmite version"#
    )]
    Version,
}

#[derive(Subcommand)]
enum PoolCommand {
    #[command(
        about = "Create one or more pools",
        long_about = r#"Create one or more pools in the pool directory."#,
        after_help = r#"EXAMPLES
  $ plasmite pool create demo
  $ plasmite pool create --size 8M demo-1 demo-2

NOTES
  - Use `--dir` to choose where pool files live."#
    )]
    Create {
        #[arg(required = true, help = "Pool name(s) to create")]
        names: Vec<String>,
        #[arg(long, help = "Pool size (bytes or K/M/G)")]
        size: Option<String>,
    },
    #[command(
        about = "Show pool metadata and bounds",
        long_about = r#"Show pool metadata and bounds as JSON."#,
        after_help = r#"EXAMPLE
  $ plasmite pool info demo"#
    )]
    Info {
        #[arg(help = "Pool name or path")]
        name: String,
    },
    #[command(
        about = "Delete a pool file",
        long_about = r#"Delete a pool file (destructive)."#,
        after_help = r#"EXAMPLE
  $ plasmite pool delete demo

NOTES
  - This permanently removes the pool file."#
    )]
    Delete {
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

fn add_missing_pool_hint(err: Error, pool_ref: &str, input: &str) -> Error {
    if err.kind() != ErrorKind::NotFound || err.hint().is_some() {
        return err;
    }
    if input.contains('/') {
        return err.with_hint(
            "Pool path not found. Check the path or pass --dir for a different pool directory.",
        );
    }
    err.with_hint(format!(
        "Create it first: plasmite pool create {pool_ref} (or pass --dir for a different pool directory)."
    ))
}

fn add_missing_seq_hint(err: Error, pool_ref: &str) -> Error {
    if err.kind() != ErrorKind::NotFound || err.seq().is_none() || err.hint().is_some() {
        return err;
    }
    err.with_hint(format!(
        "Check available messages: plasmite pool info {pool_ref} (or plasmite peek {pool_ref} --tail 10)."
    ))
}

fn add_io_hint(err: Error) -> Error {
    if err.hint().is_some() {
        return err;
    }
    match err.kind() {
        ErrorKind::Permission => err.with_hint(
            "Permission denied. Check directory permissions or use --dir to a writable location.",
        ),
        ErrorKind::Busy => {
            err.with_hint("Pool is busy (another writer holds the lock). Retry with backoff.")
        }
        ErrorKind::Io => err.with_hint("I/O error. Check the path, filesystem, and disk space."),
        _ => err,
    }
}

fn add_corrupt_hint(err: Error) -> Error {
    if err.kind() != ErrorKind::Corrupt || err.hint().is_some() {
        return err;
    }
    err.with_hint("Pool appears corrupt. Recreate it or investigate with validation tooling.")
}

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
            .with_hint("Use bytes or K/M/G (e.g. 64M).")
            .with_source(err)
    })?;

    let multiplier = match suffix {
        "" => 1,
        "K" | "k" => 1024,
        "M" | "m" => 1024 * 1024,
        "G" | "g" => 1024 * 1024 * 1024,
        _ => {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("invalid size suffix")
                .with_hint("Use K/M/G (e.g. 64M)."));
        }
    };

    value.checked_mul(multiplier).ok_or_else(|| {
        Error::new(ErrorKind::Usage)
            .with_message("size overflow")
            .with_hint("Use a smaller size value.")
    })
}

fn parse_durability(input: &str) -> Result<Durability, Error> {
    match input.trim() {
        "fast" => Ok(Durability::Fast),
        "flush" => Ok(Durability::Flush),
        _ => Err(Error::new(ErrorKind::Usage)
            .with_message("invalid durability")
            .with_hint("Use fast or flush.")),
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

#[derive(Copy, Clone, Debug)]
enum AnsiColor {
    Red,
    Yellow,
}

fn colorize_label(label: &str, enabled: bool, color: AnsiColor) -> String {
    if !enabled {
        return label.to_string();
    }
    let code = match color {
        AnsiColor::Red => "31",
        AnsiColor::Yellow => "33",
    };
    format!("\u{1b}[{code}m{label}\u{1b}[0m")
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

fn emit_error(err: &Error, color_mode: ColorMode) {
    let is_tty = io::stderr().is_terminal();
    if is_tty {
        eprintln!("{}", error_text(err, color_mode.use_color(is_tty)));
        return;
    }

    let value = error_json(err);
    let json = serde_json::to_string(&value).unwrap_or_else(|_| {
        "{\"error\":{\"kind\":\"Internal\",\"message\":\"json encode failed\"}}".to_string()
    });
    eprintln!("{json}");
}

fn notice_time_now() -> Option<String> {
    use time::format_description::well_known::Rfc3339;
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let ts = time::OffsetDateTime::from_unix_timestamp_nanos(duration.as_nanos() as i128).ok()?;
    ts.format(&Rfc3339).ok()
}

fn emit_notice(notice: &Notice, color_mode: ColorMode) {
    let is_tty = io::stderr().is_terminal();
    if is_tty {
        eprintln!(
            "{} {} (pool: {})",
            colorize_label("notice:", color_mode.use_color(is_tty), AnsiColor::Yellow),
            notice.message,
            notice.pool
        );
        return;
    }

    let value = notice_json(notice);
    let json = serde_json::to_string(&value).unwrap_or_else(|_| {
        "{\"notice\":{\"kind\":\"Internal\",\"message\":\"json encode failed\"}}".to_string()
    });
    eprintln!("{json}");
}

fn error_message(err: &Error) -> String {
    if let Some(message) = err.message() {
        return message.to_string();
    }
    match err.kind() {
        ErrorKind::Internal => "internal error".to_string(),
        ErrorKind::Usage => "usage error".to_string(),
        ErrorKind::NotFound => "not found".to_string(),
        ErrorKind::AlreadyExists => "already exists".to_string(),
        ErrorKind::Busy => "resource is busy".to_string(),
        ErrorKind::Permission => "permission denied".to_string(),
        ErrorKind::Corrupt => "corrupt data".to_string(),
        ErrorKind::Io => "i/o error".to_string(),
    }
}

fn error_causes(err: &Error) -> Vec<String> {
    let mut causes = Vec::new();
    let mut cur = err.source();
    while let Some(source) = cur {
        causes.push(source.to_string());
        cur = source.source();
    }
    causes
}

fn error_json(err: &Error) -> Value {
    let mut inner = Map::new();
    inner.insert("kind".to_string(), json!(format!("{:?}", err.kind())));
    inner.insert("message".to_string(), json!(error_message(err)));
    if let Some(hint) = err.hint() {
        inner.insert("hint".to_string(), json!(hint));
    }
    if let Some(path) = err.path() {
        inner.insert("path".to_string(), json!(path.display().to_string()));
    }
    if let Some(seq) = err.seq() {
        inner.insert("seq".to_string(), json!(seq));
    }
    if let Some(offset) = err.offset() {
        inner.insert("offset".to_string(), json!(offset));
    }
    let causes = error_causes(err);
    if !causes.is_empty() {
        inner.insert("causes".to_string(), json!(causes));
    }

    let mut outer = Map::new();
    outer.insert("error".to_string(), Value::Object(inner));
    Value::Object(outer)
}

fn error_text(err: &Error, use_color: bool) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "{} {}",
        colorize_label("error:", use_color, AnsiColor::Red),
        error_message(err)
    ));

    if let Some(hint) = err.hint() {
        lines.push(format!(
            "{} {hint}",
            colorize_label("hint:", use_color, AnsiColor::Yellow)
        ));
    }
    if let Some(path) = err.path() {
        lines.push(format!(
            "{} {}",
            colorize_label("path:", use_color, AnsiColor::Yellow),
            path.display()
        ));
    }
    if let Some(seq) = err.seq() {
        lines.push(format!(
            "{} {seq}",
            colorize_label("seq:", use_color, AnsiColor::Yellow)
        ));
    }
    if let Some(offset) = err.offset() {
        lines.push(format!(
            "{} {offset}",
            colorize_label("offset:", use_color, AnsiColor::Yellow)
        ));
    }

    let causes = error_causes(err);
    if let Some(cause) = causes.first() {
        lines.push(format!(
            "{} {cause}",
            colorize_label("caused by:", use_color, AnsiColor::Yellow)
        ));
    }

    lines.join("\n")
}

fn clap_error_summary(err: &clap::Error) -> String {
    for line in err.to_string().lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("error:") {
            return rest.trim().to_string();
        }
        return trimmed.to_string();
    }
    "invalid arguments".to_string()
}

fn clap_error_hint(err: &clap::Error) -> String {
    let rendered = err.to_string();
    let usage = rendered
        .lines()
        .find_map(|line| line.trim().strip_prefix("Usage: "))
        .map(str::trim);

    let Some(usage) = usage else {
        return "Try `plasmite --help`.".to_string();
    };

    let tokens: Vec<&str> = usage.split_whitespace().collect();
    let Some(pos) = tokens.iter().position(|t| *t == "plasmite") else {
        return "Try `plasmite --help`.".to_string();
    };

    let mut parts = Vec::new();
    for token in tokens.iter().skip(pos + 1) {
        if token.starts_with('-') || token.starts_with('<') || token.starts_with('[') {
            break;
        }
        parts.push(*token);
    }

    if parts.is_empty() {
        return "Try `plasmite --help`.".to_string();
    }

    format!("Try `plasmite {} --help`.", parts.join(" "))
}

fn read_data_single(data: Option<String>, file: Option<String>) -> Result<Value, Error> {
    let json_str = if let Some(data) = data {
        data
    } else if let Some(file) = file {
        if file == "-" {
            read_stdin()?
        } else {
            std::fs::read_to_string(&file).map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to read data file")
                    .with_path(file)
                    .with_source(err)
            })?
        }
    } else {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("missing data input")
            .with_hint("Provide JSON via DATA, --file, or pipe JSON to stdin."));
    };

    serde_json::from_str(&json_str).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid json")
            .with_hint("Provide a single JSON value (e.g. '{\"x\":1}').")
            .with_source(err)
    })
}

fn read_stdin() -> Result<String, Error> {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read stdin")
            .with_source(err)
    })?;
    Ok(buf)
}

fn read_json_stream<R, F>(reader: R, mut on_value: F) -> Result<usize, Error>
where
    R: Read,
    F: FnMut(Value) -> Result<(), Error>,
{
    let stream = serde_json::Deserializer::from_reader(reader).into_iter::<Value>();
    let mut count = 0usize;
    for item in stream {
        let value = item.map_err(|err| {
            Error::new(ErrorKind::Usage)
                .with_message("invalid json stream")
                .with_hint("Provide JSON values separated by whitespace or newlines.")
                .with_source(err)
        })?;
        on_value(value)?;
        count += 1;
    }
    Ok(count)
}

fn now_ns() -> Result<u64, Error> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("time went backwards")
                .with_source(err)
        })?;
    Ok(duration.as_nanos() as u64)
}

fn format_ts(timestamp_ns: u64) -> Result<String, Error> {
    use time::format_description::well_known::Rfc3339;
    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(timestamp_ns as i128).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("invalid timestamp")
                .with_source(err)
        })?;
    ts.format(&Rfc3339).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("timestamp format failed")
            .with_source(err)
    })
}

fn message_json(
    seq: u64,
    timestamp_ns: u64,
    descrips: &[String],
    data: &Value,
) -> Result<Value, Error> {
    let meta = json!({ "descrips": descrips });
    Ok(json!({
        "seq": seq,
        "time": format_ts(timestamp_ns)?,
        "meta": meta,
        "data": data,
    }))
}

fn message_from_frame(frame: &FrameRef<'_>) -> Result<Value, Error> {
    let (meta, data) = decode_payload(frame.payload)?;
    Ok(json!({
        "seq": frame.seq,
        "time": format_ts(frame.timestamp_ns)?,
        "meta": meta,
        "data": data,
    }))
}

fn decode_payload(payload: &[u8]) -> Result<(Value, Value), Error> {
    let json_str = Lite3DocRef::new(payload).to_json(false)?;
    let value: Value = serde_json::from_str(&json_str).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid payload json")
            .with_source(err)
    })?;
    let obj = value
        .as_object()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("payload is not object"))?;
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

#[derive(Debug, Clone)]
struct DropNotice {
    last_seen_seq: u64,
    next_seen_seq: u64,
}

impl DropNotice {
    fn dropped_count(&self) -> u64 {
        self.next_seen_seq.saturating_sub(self.last_seen_seq + 1)
    }
}

fn peek(
    pool: &Pool,
    pool_ref: &str,
    pool_path: &Path,
    tail: u64,
    pretty: bool,
    quiet_drops: bool,
    color_mode: ColorMode,
) -> Result<(), Error> {
    let mut cursor = Cursor::new();
    let mut header = pool.header_from_mmap()?;
    let mut emit = VecDeque::new();
    let mut last_seen_seq = None::<u64>;
    let mut pending_drop: Option<DropNotice> = None;
    let mut last_notice_at: Option<Instant> = None;
    let notice_interval = Duration::from_secs(1);

    if tail > 0 {
        cursor.seek_to(header.tail_off as usize);
        loop {
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    emit.push_back(message_from_frame(&frame)?);
                    last_seen_seq = Some(frame.seq);
                    while emit.len() > tail as usize {
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
    }

    if tail == 0 {
        cursor.seek_to(header.head_off as usize);
    }

    let mut backoff = Duration::from_millis(1);
    let max_backoff = Duration::from_millis(50);

    let pool_ref = pool_ref.to_string();
    let pool_path = pool_path.display().to_string();

    let maybe_emit_pending = |pending: &mut Option<DropNotice>,
                              last_notice_at: &mut Option<Instant>| {
        if quiet_drops {
            pending.take();
            return;
        }
        let Some(pending_notice) = pending.as_ref() else {
            return;
        };
        let ready = last_notice_at
            .map(|instant| instant.elapsed() >= notice_interval)
            .unwrap_or(true);
        if !ready {
            return;
        }
        let time = match notice_time_now() {
            Some(time) => time,
            None => {
                pending.take();
                return;
            }
        };
        let dropped_count = pending_notice.dropped_count();
        let mut details = Map::new();
        details.insert(
            "last_seen_seq".to_string(),
            json!(pending_notice.last_seen_seq),
        );
        details.insert(
            "next_seen_seq".to_string(),
            json!(pending_notice.next_seen_seq),
        );
        details.insert("dropped_count".to_string(), json!(dropped_count));
        details.insert("pool_path".to_string(), json!(pool_path.as_str()));
        let notice = Notice {
            kind: "drop".to_string(),
            time,
            cmd: "peek".to_string(),
            pool: pool_ref.clone(),
            message: format!("dropped {dropped_count} messages"),
            details,
        };
        emit_notice(&notice, color_mode);
        *last_notice_at = Some(Instant::now());
        pending.take();
    };

    let queue_drop = |last_seen_seq: u64, next_seen_seq: u64, pending: &mut Option<DropNotice>| {
        if quiet_drops {
            return;
        }
        match pending {
            Some(existing) => {
                existing.next_seen_seq = next_seen_seq;
            }
            None => {
                *pending = Some(DropNotice {
                    last_seen_seq,
                    next_seen_seq,
                });
            }
        }
    };

    loop {
        match cursor.next(pool)? {
            CursorResult::Message(frame) => {
                if let Some(last_seen_seq) = last_seen_seq {
                    if frame.seq > last_seen_seq + 1 {
                        queue_drop(last_seen_seq, frame.seq, &mut pending_drop);
                        maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                    }
                }
                emit_message(message_from_frame(&frame)?, pretty);
                last_seen_seq = Some(frame.seq);
                maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                backoff = Duration::from_millis(1);
            }
            CursorResult::WouldBlock => {
                maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                std::thread::sleep(backoff);
                backoff = std::cmp::min(backoff * 2, max_backoff);
            }
            CursorResult::FellBehind => {
                header = pool.header_from_mmap()?;
                if tail > 0 {
                    cursor.seek_to(header.tail_off as usize);
                } else {
                    cursor.seek_to(header.head_off as usize);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, ErrorKind, error_text, parse_size, read_json_stream};
    use serde_json::json;
    use std::io::Cursor;

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

    #[test]
    fn read_json_stream_accepts_multiple_values() {
        let input = b"{\"a\":1}\n {\"b\":2} {\"c\":3}";
        let mut values = Vec::new();
        let count = read_json_stream(Cursor::new(input), |value| {
            values.push(value);
            Ok(())
        })
        .expect("stream parse");
        assert_eq!(count, 3);
        assert_eq!(values, vec![json!({"a":1}), json!({"b":2}), json!({"c":3})]);
    }

    #[test]
    fn error_text_respects_color_flag() {
        let err = Error::new(ErrorKind::Usage).with_message("bad input");
        let colored = error_text(&err, true);
        let plain = error_text(&err, false);
        assert!(colored.contains("\u{1b}[31merror:\u{1b}[0m"));
        assert!(plain.contains("error:"));
        assert!(!plain.contains("\u{1b}["));
    }
}
