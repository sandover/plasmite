//! Purpose: `plasmite` CLI entry point and v0.0.1 command dispatch.
//! Role: Binary crate root; parses args, runs commands, emits JSON on stdout.
//! Invariants: Successful command output is JSON on stdout; errors are JSON on stderr.
//! Invariants: Process exit code is derived from `api::to_exit_code`.
//! Invariants: All pool mutations go through `api::Pool` (locks + mmap safety).
#![allow(clippy::result_large_err)]
use std::ffi::OsString;
use std::io::{self, IsTerminal, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum, error::ErrorKind as ClapErrorKind};
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::error::Error as StdError;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod color_json;
mod ingest;
mod jq_filter;
mod serve;

use color_json::colorize_json;
use ingest::{ErrorPolicy, IngestConfig, IngestFailure, IngestMode, IngestOutcome, ingest};
use jq_filter::{JqFilter, compile_filters, matches_all};
use plasmite::api::{
    AppendOptions, Cursor, CursorResult, Durability, Error, ErrorKind, FrameRef, Lite3DocRef,
    LocalClient, Pool, PoolOptions, PoolRef, ValidationIssue, ValidationReport, ValidationStatus,
    lite3, to_exit_code,
};
use plasmite::notice::{Notice, notice_json};

#[derive(Copy, Clone, Debug)]
struct RunOutcome {
    exit_code: i32,
}

impl RunOutcome {
    fn ok() -> Self {
        Self { exit_code: 0 }
    }

    fn with_code(exit_code: i32) -> Self {
        Self { exit_code }
    }
}

fn main() {
    let exit_code = match run() {
        Ok(outcome) => outcome.exit_code,
        Err((err, color_mode)) => {
            emit_error(&err, color_mode);
            to_exit_code(err.kind())
        }
    };
    std::process::exit(exit_code);
}

fn run() -> Result<RunOutcome, (Error, ColorMode)> {
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
                return Ok(RunOutcome::ok());
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
            emit_json(output, color_mode);
            Ok(RunOutcome::ok())
        }
        Command::Doctor { pool, all } => {
            if all && pool.is_some() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("--all cannot be combined with a pool name")
                    .with_hint("Use --all by itself, or provide a single pool."));
            }
            let client = LocalClient::new().with_pool_dir(&pool_dir);
            let reports = if let Some(pool) = pool {
                let path = resolve_poolref(&pool, &pool_dir)?;
                let pool_ref = PoolRef::path(path.clone());
                vec![doctor_report(&client, pool_ref, pool, path)?]
            } else {
                let pools = client.list_pools()?;
                let mut reports = Vec::new();
                for info in pools {
                    let label = info.path.to_string_lossy().to_string();
                    let pool_ref = PoolRef::path(info.path.clone());
                    reports.push(doctor_report(&client, pool_ref, label, info.path)?);
                }
                reports
            };

            let is_tty = std::io::stdout().is_terminal();
            if is_tty {
                for report in &reports {
                    emit_doctor_human(report);
                }
            } else {
                let values = reports.iter().map(report_json).collect::<Vec<_>>();
                emit_json(json!({ "reports": values }), color_mode);
            }

            let has_corrupt = reports
                .iter()
                .any(|report| report.status == ValidationStatus::Corrupt);
            let exit_code = if has_corrupt {
                to_exit_code(ErrorKind::Corrupt)
            } else {
                0
            };
            Ok(RunOutcome::with_code(exit_code))
        }
        Command::Serve { bind, token } => {
            let bind: SocketAddr = bind
                .parse()
                .map_err(|_| Error::new(ErrorKind::Usage).with_message("invalid bind address"))?;
            let config = serve::ServeConfig {
                bind,
                pool_dir: pool_dir.clone(),
                token,
            };
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|err| {
                    Error::new(ErrorKind::Internal)
                        .with_message("failed to start runtime")
                        .with_source(err)
                })?;
            runtime.block_on(serve::serve(config))?;
            Ok(RunOutcome::ok())
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
                emit_json(json!({ "created": results }), color_mode);
                Ok(RunOutcome::ok())
            }
            PoolCommand::Info { name } => {
                let path = resolve_poolref(&name, &pool_dir)?;
                let pool =
                    Pool::open(&path).map_err(|err| add_missing_pool_hint(err, &name, &name))?;
                let info = pool.info()?;
                emit_json(pool_info_json(&name, &info), color_mode);
                Ok(RunOutcome::ok())
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
                emit_json(
                    json!({
                        "deleted": {
                            "pool": name,
                            "path": path.display().to_string(),
                        }
                    }),
                    color_mode,
                );
                Ok(RunOutcome::ok())
            }
            PoolCommand::List => {
                let pools = list_pools(&pool_dir);
                emit_json(json!({ "pools": pools }), color_mode);
                Ok(RunOutcome::ok())
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
            retry,
            retry_delay,
            input,
            errors,
        } => {
            let path = resolve_poolref(&pool, &pool_dir)?;
            if create_size.is_some() && !create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("--create-size requires --create")
                    .with_hint("Add --create or remove --create-size."));
            }
            if retry_delay.is_some() && retry == 0 {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("--retry-delay requires --retry")
                    .with_hint("Add --retry or remove --retry-delay."));
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
            let retry_config = parse_retry_config(retry, retry_delay.as_deref())?;
            if data.is_some() && file.is_some() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("multiple data inputs provided")
                    .with_hint("Use only one of DATA, --file, or stdin."));
            }

            if data.is_some() || file.is_some() || io::stdin().is_terminal() {
                let data = read_data_single(data, file)?;
                let payload = lite3::encode_message(&descrip, &data)?;
                let (seq, timestamp_ns) = retry_with_config(retry_config, || {
                    let timestamp_ns = now_ns()?;
                    let options = AppendOptions::new(timestamp_ns, durability);
                    let seq = pool_handle.append_with_options(payload.as_slice(), options)?;
                    Ok((seq, timestamp_ns))
                })?;
                emit_json(
                    message_json(seq, timestamp_ns, &descrip, &data)?,
                    color_mode,
                );
            } else {
                let outcome = ingest_from_stdin(
                    io::stdin().lock(),
                    PokeIngestContext {
                        pool_ref: &pool,
                        pool_path: &path,
                        descrips: &descrip,
                        durability,
                        retry_config,
                        pool_handle: &mut pool_handle,
                        color_mode,
                        input,
                        errors,
                    },
                )?;
                if outcome.records_total == 0 {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("missing data input")
                        .with_hint("Provide JSON via DATA, --file, or pipe JSON to stdin."));
                }
                if outcome.failed > 0 {
                    return Ok(RunOutcome::with_code(1));
                }
            }
            Ok(RunOutcome::ok())
        }
        Command::Get { pool, seq } => {
            let path = resolve_poolref(&pool, &pool_dir)?;
            let pool_handle =
                Pool::open(&path).map_err(|err| add_missing_pool_hint(err, &pool, &pool))?;
            let frame = pool_handle
                .get(seq)
                .map_err(|err| add_missing_seq_hint(err, &pool))?;
            emit_json(message_from_frame(&frame)?, color_mode);
            Ok(RunOutcome::ok())
        }
        Command::Peek {
            pool,
            jsonl,
            tail,
            one,
            timeout,
            data_only,
            quiet_drops,
            format,
            since,
            where_expr,
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
            let now = now_ns()?;
            let since_ns = since
                .as_deref()
                .map(|value| parse_since(value, now))
                .transpose()?;
            if let Some(since_ns) = since_ns {
                if since_ns > now {
                    return Ok(RunOutcome::ok());
                }
            }
            let timeout = timeout.as_deref().map(parse_duration).transpose()?;
            let cfg = PeekConfig {
                tail,
                pretty,
                one,
                timeout,
                data_only,
                since_ns,
                where_predicates: compile_filters(&where_expr)?,
                quiet_drops,
                color_mode,
            };
            let outcome = peek(&pool_handle, &pool, &path, cfg)?;
            Ok(outcome)
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
    about = "Persistent message pools for local IPC",
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
    before_help = r#"Multiple processes can write and read concurrently. Messages are JSON.

Mental model:
  - `poke` sends messages (write)
  - `peek` watches messages (read/stream)
  - `get` fetches one message by seq
"#,
    after_help = r#"EXAMPLE
  $ plasmite pool create chat
  $ plasmite peek chat              # Terminal 1: bob watches (waits for messages)
  $ plasmite poke chat '{"from": "alice", "msg": "hello"}'   # Terminal 2: alice sends
  # bob sees: {"seq":1,"time":"...","meta":{"descrips":[]},"data":{"from":"alice","msg":"hello"}}

LEARN MORE
  $ plasmite <command> --help
  https://github.com/sandover/plasmite"#,
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
struct Cli {
    #[arg(
        long,
        help = "Pool directory for named pools (default: ~/.plasmite/pools)"
    )]
    dir: Option<PathBuf>,
    #[arg(
        long,
        default_value = "auto",
        value_enum,
        help = "Colorize stderr diagnostics and pretty JSON output: auto|always|never"
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

#[derive(Copy, Clone, Debug, ValueEnum)]
enum InputMode {
    Auto,
    Jsonl,
    Json,
    Seq,
    Jq,
}

#[derive(Copy, Clone, Debug, PartialEq, ValueEnum)]
enum ErrorPolicyCli {
    Stop,
    Skip,
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
        long_about = r#"Create and inspect pool files.

Pools are persistent ring buffers: multiple writers, multiple readers, crash-safe."#,
        after_help = r#"EXAMPLES
  $ plasmite pool create foo
  $ plasmite pool create --size 8M bar baz
  $ plasmite pool info foo
  $ plasmite pool list
  $ plasmite pool delete foo

NOTES
  - Default location: ~/.plasmite/pools (override with --dir)"#
    )]
    Pool {
        #[command(subcommand)]
        command: PoolCommand,
    },
    #[command(
        about = "Send a message to a pool",
        long_about = r#"Send JSON messages to a pool.

Accepts inline JSON, a file (--file), or streams via stdin (auto-detected)."#,
        after_help = r#"EXAMPLES
  # Inline JSON
  $ plasmite poke foo '{"hello": "world"}'

  # Tag messages with --descrip
  $ plasmite poke foo --descrip ping --descrip from-alice '{"msg": "hello bob"}'

  # Pipe JSON Lines
  $ jq -c '.items[]' data.json | plasmite poke foo

  # Stream from curl (event streams auto-detected)
  $ curl -N https://api.example.com/events | plasmite poke events

  # Auto-create pool on first poke
  $ plasmite poke bar --create '{"first": "message"}'

NOTES
  - `--in auto` detects JSONL, JSON-seq (0x1e), event streams (data: prefix)
  - `--errors skip` continues past bad records; `--durability flush` syncs to disk
  - `--retry N` retries on transient failures (lock contention, etc.)"#
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
        #[arg(long, default_value_t = 0, help = "Retry count for transient failures")]
        retry: u32,
        #[arg(long, help = "Delay between retries (e.g. 50ms, 1s, 2m)")]
        retry_delay: Option<String>,
        #[arg(
            short = 'i',
            long = "in",
            default_value = "auto",
            value_enum,
            help = "Input mode for stdin streams"
        )]
        input: InputMode,
        #[arg(
            short = 'e',
            long = "errors",
            default_value = "stop",
            value_enum,
            help = "Stream error policy: stop|skip"
        )]
        errors: ErrorPolicyCli,
    },
    #[command(
        about = "Serve pools over HTTP (loopback-only in v0)",
        long_about = r#"Serve pools over HTTP (loopback-only in v0).

Implements the remote protocol spec under spec/remote/v0/SPEC.md."#,
        after_help = r#"EXAMPLES
  $ plasmite serve
  $ plasmite serve --bind 127.0.0.1:9701 --token devtoken

NOTES
  - v0 is loopback-only; non-loopback binds are rejected
  - Use Authorization: Bearer <token> when --token is set"#
    )]
    Serve {
        #[arg(long, default_value = "127.0.0.1:9700", help = "Bind address")]
        bind: String,
        #[arg(long, help = "Bearer token for auth (optional on loopback)")]
        token: Option<String>,
    },
    #[command(
        about = "Fetch one message by sequence number",
        long_about = r#"Fetch a specific message by its seq number and print as JSON."#,
        after_help = r#"EXAMPLES
  $ plasmite get foo 1
  $ plasmite get foo 42 | jq '.data'"#
    )]
    Get {
        #[arg(help = "Pool name or path")]
        pool: String,
        #[arg(help = "Sequence number")]
        seq: u64,
    },
    #[command(
        about = "Watch messages from a pool",
        long_about = r#"Watch a pool and stream messages as they arrive.

By default, `peek` waits for new messages forever (Ctrl-C to stop).
Use `--tail N` to see recent history first, then keep watching."#,
        after_help = r#"EXAMPLES
  # Watch for new messages
  $ plasmite peek foo

  # Last 10 messages, then keep watching
  $ plasmite peek foo --tail 10

  # Emit one matching message, then exit
  $ plasmite peek foo --where '.data.status == "error"' --one

  # Messages from the last 5 minutes
  $ plasmite peek foo --since 5m

  # Filter by tag (descrip)
  $ plasmite peek foo --where '.meta.descrips[]? == "ping"'

  # Filter by data field
  $ plasmite peek foo --where '.data.status == "error"'

  # Pipe to jq
  $ plasmite peek foo --format jsonl | jq -r '.data.msg'

  # Wait up to 5 seconds for a message
  $ plasmite peek foo --timeout 5s

  # Emit only data payloads
  $ plasmite peek foo --data-only --format jsonl

NOTES
  - Use `--format jsonl` for scripts (one JSON object per line)
  - `--where` uses jq-style expressions; repeat for AND
  - `--since 5m` and `--since 2026-01-15T10:00:00Z` both work"#
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
        #[arg(long, help = "Exit after emitting one matching message")]
        one: bool,
        #[arg(long, help = "Emit JSON Lines (one object per line)")]
        jsonl: bool,
        #[arg(
            long,
            help = "Exit 124 if no output within duration (e.g. 500ms, 5s, 1m)"
        )]
        timeout: Option<String>,
        #[arg(long, help = "Emit only the .data payload")]
        data_only: bool,
        #[arg(
            long,
            value_enum,
            help = "Output format: pretty|jsonl (use --jsonl as alias for jsonl)"
        )]
        format: Option<PeekFormat>,
        #[arg(
            long,
            help = "Only emit messages at or after this time (RFC 3339 or relative like 5m)",
            conflicts_with = "tail"
        )]
        since: Option<String>,
        #[arg(
            long = "where",
            value_name = "EXPR",
            help = "Filter messages by boolean expression (repeatable; AND across repeats)"
        )]
        where_expr: Vec<String>,
        #[arg(long = "quiet-drops", help = "Suppress drop notices on stderr")]
        quiet_drops: bool,
    },
    #[command(
        about = "Diagnose pool health",
        long_about = r#"Validate one pool (or all pools) and emit a diagnostic report."#,
        after_help = r#"EXAMPLES
  $ plasmite doctor foo
  $ plasmite doctor --all

NOTES
  - Outputs JSON when stdout is not a TTY.
  - Exits nonzero when corruption is detected."#
    )]
    Doctor {
        #[arg(help = "Pool name or path", required = false)]
        pool: Option<String>,
        #[arg(long, help = "Validate all pools in the pool directory")]
        all: bool,
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
        long_about = r#"Create pool files. Default size is 1MB (use --size for larger)."#,
        after_help = r#"EXAMPLES
  $ plasmite pool create foo
  $ plasmite pool create --size 8M bar baz quux

NOTES
  - Sizes: 64K, 1M, 8M, 1G (K/M/G are 1024-based)"#
    )]
    Create {
        #[arg(required = true, help = "Pool name(s) to create")]
        names: Vec<String>,
        #[arg(long, help = "Pool size (bytes or K/M/G)")]
        size: Option<String>,
    },
    #[command(
        about = "Show pool metadata and bounds",
        long_about = r#"Show pool size, message count, and sequence bounds as JSON."#,
        after_help = r#"EXAMPLE
  $ plasmite pool info foo"#
    )]
    Info {
        #[arg(help = "Pool name or path")]
        name: String,
    },
    #[command(
        about = "Delete a pool file",
        long_about = r#"Delete a pool file (destructive, cannot be undone)."#,
        after_help = r#"EXAMPLE
  $ plasmite pool delete foo"#
    )]
    Delete {
        #[arg(help = "Pool name or path")]
        name: String,
    },
    #[command(
        about = "List pools in the pool directory",
        long_about = r#"List pools in the pool directory as JSON."#,
        after_help = r#"EXAMPLE
  $ plasmite pool list

NOTES
  - Output is JSON (pretty on TTY, compact when piped).
  - Non-.plasmite files are ignored.
  - Pools that cannot be read include an error field."#
    )]
    List,
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
const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(50);
const DEFAULT_SNIFF_BYTES: usize = 8 * 1024;
const DEFAULT_SNIFF_LINES: usize = 8;
const DEFAULT_MAX_RECORD_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_SNIPPET_BYTES: usize = 200;

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

fn emit_doctor_human(report: &ValidationReport) {
    let label = report
        .pool_ref
        .clone()
        .unwrap_or_else(|| report.path.to_string_lossy().to_string());
    match report.status {
        ValidationStatus::Ok => {
            println!("OK: {label}");
        }
        ValidationStatus::Corrupt => {
            let last_good = report
                .last_good_seq
                .map(|seq| format!(" last_good_seq={seq}"))
                .unwrap_or_default();
            let issue = report
                .issues
                .first()
                .map(|issue| format!(" issue={}", issue.message))
                .unwrap_or_default();
            println!("CORRUPT: {label}{last_good}{issue}");
        }
    }
}

fn report_json(report: &ValidationReport) -> Value {
    let issues = report
        .issues
        .iter()
        .map(|issue| {
            json!({
                "code": issue.code,
                "message": issue.message,
                "seq": issue.seq,
                "offset": issue.offset,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "pool_ref": report.pool_ref,
        "path": report.path.to_string_lossy(),
        "status": match report.status {
            ValidationStatus::Ok => "ok",
            ValidationStatus::Corrupt => "corrupt",
        },
        "last_good_seq": report.last_good_seq,
        "issue_count": report.issue_count,
        "issues": issues,
        "remediation_hints": report.remediation_hints,
        "snapshot_path": report.snapshot_path.as_ref().map(|path| path.to_string_lossy()),
    })
}

fn doctor_report(
    client: &LocalClient,
    pool_ref: PoolRef,
    label: String,
    path: PathBuf,
) -> Result<ValidationReport, Error> {
    match client.validate_pool(&pool_ref) {
        Ok(report) => Ok(report.with_pool_ref(label)),
        Err(err) if err.kind() == ErrorKind::Corrupt => {
            Ok(ValidationReport::corrupt(path, error_issue(&err), None).with_pool_ref(label))
        }
        Err(err) => Err(err),
    }
}

fn error_issue(err: &Error) -> ValidationIssue {
    ValidationIssue {
        code: "corrupt".to_string(),
        message: err.message().unwrap_or("corrupt").to_string(),
        seq: err.seq(),
        offset: err.offset(),
    }
}

fn list_pools(pool_dir: &Path) -> Vec<Value> {
    let mut pools = Vec::new();
    let entries = match std::fs::read_dir(pool_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return pools,
        Err(err) => {
            pools.push(pool_list_error(
                "pools",
                pool_dir,
                Error::new(ErrorKind::Io)
                    .with_message("failed to read pool directory")
                    .with_path(pool_dir)
                    .with_source(err),
            ));
            return pools;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("plasmite") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown")
            .to_string();
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) => {
                pools.push(pool_list_error(
                    &name,
                    &path,
                    Error::new(ErrorKind::Io)
                        .with_message("failed to stat pool")
                        .with_path(&path)
                        .with_source(err),
                ));
                continue;
            }
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(format_system_time)
            .map(Value::String)
            .unwrap_or(Value::Null);
        match Pool::open(&path) {
            Ok(pool) => {
                let info = match pool.info() {
                    Ok(info) => info,
                    Err(err) => {
                        pools.push(pool_list_error(
                            &name,
                            &path,
                            add_corrupt_hint(add_io_hint(err)),
                        ));
                        continue;
                    }
                };
                let mut map = Map::new();
                map.insert("name".to_string(), json!(name));
                map.insert("path".to_string(), json!(path.display().to_string()));
                map.insert("file_size".to_string(), json!(info.file_size));
                map.insert("bounds".to_string(), bounds_json(info.bounds));
                map.insert("mtime".to_string(), mtime);
                pools.push(Value::Object(map));
            }
            Err(err) => {
                pools.push(pool_list_error(
                    &name,
                    &path,
                    add_corrupt_hint(add_io_hint(err)),
                ));
            }
        }
    }

    pools.sort_by_key(pool_list_name);
    pools
}

fn pool_list_error(name: &str, path: &Path, err: Error) -> Value {
    let mut map = Map::new();
    map.insert("name".to_string(), json!(name));
    map.insert("path".to_string(), json!(path.display().to_string()));
    map.insert("error".to_string(), error_json(&err));
    Value::Object(map)
}

fn pool_list_name(value: &Value) -> String {
    value
        .get("name")
        .and_then(|name| name.as_str())
        .unwrap_or("")
        .to_string()
}

fn format_system_time(time: std::time::SystemTime) -> Option<String> {
    use time::format_description::well_known::Rfc3339;
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    let ts = time::OffsetDateTime::from_unix_timestamp_nanos(duration.as_nanos() as i128).ok()?;
    ts.format(&Rfc3339).ok()
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

fn parse_since(input: &str, now_ns: u64) -> Result<u64, Error> {
    if let Some(duration_ns) = parse_relative_since(input) {
        return Ok(now_ns.saturating_sub(duration_ns));
    }
    let trimmed = input.trim();
    let ts = time::OffsetDateTime::parse(trimmed, &time::format_description::well_known::Rfc3339)
        .map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid --since value")
            .with_hint("Use RFC 3339 (2026-02-02T23:45:00Z) or relative like 5m.")
            .with_source(err)
    })?;
    Ok(ts.unix_timestamp_nanos() as u64)
}

fn parse_relative_since(input: &str) -> Option<u64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (digits, unit) = trimmed.split_at(trimmed.len().saturating_sub(1));
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let value: u64 = digits.parse().ok()?;
    let seconds = match unit {
        "s" | "S" => value,
        "m" | "M" => value.saturating_mul(60),
        "h" | "H" => value.saturating_mul(60 * 60),
        "d" | "D" => value.saturating_mul(60 * 60 * 24),
        _ => return None,
    };
    Some(seconds.saturating_mul(1_000_000_000))
}

#[derive(Copy, Clone, Debug)]
struct RetryConfig {
    retries: u32,
    delay: Duration,
}

fn parse_retry_config(retry: u32, retry_delay: Option<&str>) -> Result<Option<RetryConfig>, Error> {
    if retry == 0 {
        return Ok(None);
    }
    let delay = match retry_delay {
        Some(value) => parse_duration(value)?,
        None => DEFAULT_RETRY_DELAY,
    };
    Ok(Some(RetryConfig {
        retries: retry,
        delay,
    }))
}

fn parse_duration(input: &str) -> Result<Duration, Error> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("invalid duration")
            .with_hint("Use a number plus ms|s|m|h (e.g. 10s)."));
    }
    let split = trimmed.char_indices().find(|(_, ch)| !ch.is_ascii_digit());
    let (num_str, unit) = match split {
        Some((idx, _)) => trimmed.split_at(idx),
        None => ("", ""),
    };
    if num_str.is_empty() || unit.is_empty() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("invalid duration")
            .with_hint("Use a number plus ms|s|m|h (e.g. 10s)."));
    }
    let value: u64 = num_str.parse().map_err(|_| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid duration")
            .with_hint("Use a number plus ms|s|m|h (e.g. 10s).")
    })?;
    let millis = match unit {
        "ms" => value,
        "s" => value.saturating_mul(1_000),
        "m" => value.saturating_mul(60_000),
        "h" => value.saturating_mul(3_600_000),
        _ => {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("invalid duration")
                .with_hint("Use a number plus ms|s|m|h (e.g. 10s)."));
        }
    };
    Ok(Duration::from_millis(millis))
}

fn is_retryable(err: &Error) -> bool {
    match err.kind() {
        ErrorKind::Busy => true,
        ErrorKind::Io => err
            .source()
            .and_then(|source| source.downcast_ref::<io::Error>())
            .is_some_and(|io_err| {
                matches!(
                    io_err.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                )
            }),
        _ => false,
    }
}

fn add_retry_hint(err: Error, attempts: u32, waited: Duration) -> Error {
    let info = format!(
        "Retry attempts: {attempts} (waited {}ms).",
        waited.as_millis()
    );
    if let Some(hint) = err.hint().map(|hint| hint.to_string()) {
        err.with_hint(format!("{hint} {info}"))
    } else {
        err.with_hint(info)
    }
}

fn retry_with_config<T, F>(config: Option<RetryConfig>, mut f: F) -> Result<T, Error>
where
    F: FnMut() -> Result<T, Error>,
{
    let Some(config) = config else {
        return f();
    };
    let mut attempts = 0u32;
    let mut waited = Duration::from_millis(0);
    loop {
        attempts += 1;
        match f() {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempts <= config.retries && is_retryable(&err) {
                    std::thread::sleep(config.delay);
                    waited += config.delay;
                    continue;
                }
                if attempts > 1 {
                    return Err(add_retry_hint(err, attempts, waited));
                }
                return Err(err);
            }
        }
    }
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

fn bounds_json(bounds: plasmite::api::Bounds) -> Value {
    let mut map = Map::new();
    if let Some(oldest) = bounds.oldest_seq {
        map.insert("oldest".to_string(), json!(oldest));
    }
    if let Some(newest) = bounds.newest_seq {
        map.insert("newest".to_string(), json!(newest));
    }
    Value::Object(map)
}

fn pool_info_json(pool_ref: &str, info: &plasmite::api::PoolInfo) -> Value {
    let mut map = Map::new();
    map.insert("name".to_string(), json!(pool_ref));
    map.insert("path".to_string(), json!(info.path.display().to_string()));
    map.insert("file_size".to_string(), json!(info.file_size));
    map.insert("ring_offset".to_string(), json!(info.ring_offset));
    map.insert("ring_size".to_string(), json!(info.ring_size));
    map.insert("bounds".to_string(), bounds_json(info.bounds));
    Value::Object(map)
}

fn emit_json(value: serde_json::Value, color_mode: ColorMode) {
    let is_tty = io::stdout().is_terminal();
    let use_color = color_mode.use_color(is_tty);
    let pretty = is_tty || use_color;
    let json = if pretty {
        if use_color {
            colorize_json(&value, true)
        } else {
            serde_json::to_string_pretty(&value)
                .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string())
        }
    } else {
        serde_json::to_string(&value)
            .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string())
    };
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

fn emit_message(value: serde_json::Value, pretty: bool, color_mode: ColorMode) {
    let is_tty = io::stdout().is_terminal();
    let use_color = color_mode.use_color(is_tty);
    let json = if pretty {
        if use_color {
            colorize_json(&value, true)
        } else {
            serde_json::to_string_pretty(&value)
                .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string())
        }
    } else {
        serde_json::to_string(&value)
            .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string())
    };
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

fn input_mode_to_ingest(mode: InputMode) -> IngestMode {
    match mode {
        InputMode::Auto => IngestMode::Auto,
        InputMode::Jsonl => IngestMode::Jsonl,
        InputMode::Json => IngestMode::Json,
        InputMode::Seq => IngestMode::Seq,
        InputMode::Jq => IngestMode::Jq,
    }
}

fn error_policy_to_ingest(policy: ErrorPolicyCli) -> ErrorPolicy {
    match policy {
        ErrorPolicyCli::Stop => ErrorPolicy::Stop,
        ErrorPolicyCli::Skip => ErrorPolicy::Skip,
    }
}

fn ingest_failure_notice(
    failure: &IngestFailure,
    pool_ref: &str,
    pool_path: &Path,
    color_mode: ColorMode,
) {
    let mut details = Map::new();
    details.insert("mode".to_string(), json!(mode_label(failure.mode)));
    details.insert("index".to_string(), json!(failure.index));
    details.insert("error_kind".to_string(), json!(failure.error_kind));
    details.insert(
        "pool_path".to_string(),
        json!(pool_path.display().to_string()),
    );
    if let Some(line) = failure.line {
        details.insert("line".to_string(), json!(line));
    }
    if let Some(snippet) = &failure.snippet {
        details.insert("snippet".to_string(), json!(snippet));
    }
    let notice = Notice {
        kind: "ingest_skip".to_string(),
        time: notice_time_now().unwrap_or_else(|| "unknown".to_string()),
        cmd: "poke".to_string(),
        pool: pool_ref.to_string(),
        message: failure.message.clone(),
        details,
    };
    emit_notice(&notice, color_mode);
}

fn ingest_summary_notice(
    outcome: &IngestOutcome,
    pool_ref: &str,
    pool_path: &Path,
    color_mode: ColorMode,
) {
    let mut details = Map::new();
    details.insert("total".to_string(), json!(outcome.records_total));
    details.insert("ok".to_string(), json!(outcome.ok));
    details.insert("failed".to_string(), json!(outcome.failed));
    details.insert(
        "pool_path".to_string(),
        json!(pool_path.display().to_string()),
    );
    let notice = Notice {
        kind: "ingest_summary".to_string(),
        time: notice_time_now().unwrap_or_else(|| "unknown".to_string()),
        cmd: "poke".to_string(),
        pool: pool_ref.to_string(),
        message: "ingestion completed with skipped records".to_string(),
        details,
    };
    emit_notice(&notice, color_mode);
}

fn mode_label(mode: IngestMode) -> &'static str {
    match mode {
        IngestMode::Auto => "auto",
        IngestMode::Jsonl => "jsonl",
        IngestMode::Json => "json",
        IngestMode::Seq => "seq",
        IngestMode::Jq => "jq",
        IngestMode::Event => "event",
    }
}

struct PokeIngestContext<'a> {
    pool_ref: &'a str,
    pool_path: &'a Path,
    descrips: &'a [String],
    durability: Durability,
    retry_config: Option<RetryConfig>,
    pool_handle: &'a mut Pool,
    color_mode: ColorMode,
    input: InputMode,
    errors: ErrorPolicyCli,
}

fn ingest_from_stdin<R: Read>(
    reader: R,
    ctx: PokeIngestContext<'_>,
) -> Result<IngestOutcome, Error> {
    let ingest_config = IngestConfig {
        mode: input_mode_to_ingest(ctx.input),
        errors: error_policy_to_ingest(ctx.errors),
        sniff_bytes: DEFAULT_SNIFF_BYTES,
        sniff_lines: DEFAULT_SNIFF_LINES,
        max_record_bytes: DEFAULT_MAX_RECORD_BYTES,
        max_snippet_bytes: DEFAULT_MAX_SNIPPET_BYTES,
    };

    let outcome = ingest(
        reader,
        ingest_config,
        |data| {
            let payload = lite3::encode_message(ctx.descrips, &data)?;
            let (seq, timestamp_ns) = retry_with_config(ctx.retry_config, || {
                let timestamp_ns = now_ns()?;
                let options = AppendOptions::new(timestamp_ns, ctx.durability);
                let seq = ctx
                    .pool_handle
                    .append_with_options(payload.as_slice(), options)?;
                Ok((seq, timestamp_ns))
            })?;
            emit_message(
                message_json(seq, timestamp_ns, ctx.descrips, &data)?,
                false,
                ctx.color_mode,
            );
            Ok(())
        },
        |failure| ingest_failure_notice(&failure, ctx.pool_ref, ctx.pool_path, ctx.color_mode),
    )?;

    if ctx.errors == ErrorPolicyCli::Skip && outcome.failed > 0 {
        ingest_summary_notice(&outcome, ctx.pool_ref, ctx.pool_path, ctx.color_mode);
    }

    Ok(outcome)
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

fn output_value(message: Value, data_only: bool) -> Value {
    if data_only {
        message.get("data").cloned().unwrap_or(Value::Null)
    } else {
        message
    }
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

#[derive(Clone, Debug)]
struct PeekConfig {
    tail: u64,
    pretty: bool,
    one: bool,
    timeout: Option<Duration>,
    data_only: bool,
    since_ns: Option<u64>,
    where_predicates: Vec<JqFilter>,
    quiet_drops: bool,
    color_mode: ColorMode,
}

fn peek(
    pool: &Pool,
    pool_ref: &str,
    pool_path: &Path,
    cfg: PeekConfig,
) -> Result<RunOutcome, Error> {
    let mut cursor = Cursor::new();
    let mut header = pool.header_from_mmap()?;
    let mut emit = VecDeque::new();
    let mut last_seen_seq = None::<u64>;
    let mut pending_drop: Option<DropNotice> = None;
    let mut last_notice_at: Option<Instant> = None;
    let notice_interval = Duration::from_secs(1);
    let tail_wait = cfg.one && cfg.tail > 0;
    let mut timeout_deadline = cfg.timeout.map(|duration| Instant::now() + duration);

    let bump_timeout = |deadline: &mut Option<Instant>| {
        if let Some(duration) = cfg.timeout {
            *deadline = Some(Instant::now() + duration);
        }
    };

    if let Some(since_ns) = cfg.since_ns {
        cursor.seek_to(header.tail_off as usize);
        loop {
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    if frame.timestamp_ns >= since_ns {
                        let message = message_from_frame(&frame)?;
                        if matches_all(cfg.where_predicates.as_slice(), &message)? {
                            emit_message(
                                output_value(message, cfg.data_only),
                                cfg.pretty,
                                cfg.color_mode,
                            );
                            bump_timeout(&mut timeout_deadline);
                            if cfg.one {
                                return Ok(RunOutcome::ok());
                            }
                        }
                        last_seen_seq = Some(frame.seq);
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
    } else if cfg.tail > 0 {
        cursor.seek_to(header.tail_off as usize);
        loop {
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    let message = message_from_frame(&frame)?;
                    if matches_all(cfg.where_predicates.as_slice(), &message)? {
                        emit.push_back(message);
                    }
                    last_seen_seq = Some(frame.seq);
                    while emit.len() > cfg.tail as usize {
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
        if tail_wait {
            if emit.len() >= cfg.tail as usize {
                if let Some(value) = emit.back() {
                    emit_message(
                        output_value(value.clone(), cfg.data_only),
                        cfg.pretty,
                        cfg.color_mode,
                    );
                }
                return Ok(RunOutcome::ok());
            }
        } else {
            for value in emit.drain(..) {
                emit_message(
                    output_value(value, cfg.data_only),
                    cfg.pretty,
                    cfg.color_mode,
                );
                bump_timeout(&mut timeout_deadline);
            }
        }
    }

    if cfg.since_ns.is_none() && cfg.tail == 0 {
        cursor.seek_to(header.head_off as usize);
    }

    let mut backoff = Duration::from_millis(1);
    let max_backoff = Duration::from_millis(50);

    let pool_ref = pool_ref.to_string();
    let pool_path = pool_path.display().to_string();

    let maybe_emit_pending = |pending: &mut Option<DropNotice>,
                              last_notice_at: &mut Option<Instant>| {
        if cfg.quiet_drops {
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
        emit_notice(&notice, cfg.color_mode);
        *last_notice_at = Some(Instant::now());
        pending.take();
    };

    let queue_drop = |last_seen_seq: u64, next_seen_seq: u64, pending: &mut Option<DropNotice>| {
        if cfg.quiet_drops {
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
                let message = message_from_frame(&frame)?;
                if matches_all(cfg.where_predicates.as_slice(), &message)? {
                    if tail_wait {
                        emit.push_back(message);
                        while emit.len() > cfg.tail as usize {
                            emit.pop_front();
                        }
                        if emit.len() == cfg.tail as usize {
                            if let Some(value) = emit.back() {
                                emit_message(
                                    output_value(value.clone(), cfg.data_only),
                                    cfg.pretty,
                                    cfg.color_mode,
                                );
                            }
                            return Ok(RunOutcome::ok());
                        }
                    } else {
                        emit_message(
                            output_value(message, cfg.data_only),
                            cfg.pretty,
                            cfg.color_mode,
                        );
                        bump_timeout(&mut timeout_deadline);
                        if cfg.one {
                            return Ok(RunOutcome::ok());
                        }
                    }
                }
                last_seen_seq = Some(frame.seq);
                maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                backoff = Duration::from_millis(1);
            }
            CursorResult::WouldBlock => {
                maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                if let Some(deadline) = timeout_deadline {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(RunOutcome::with_code(124));
                    }
                    let remaining = deadline.duration_since(now);
                    std::thread::sleep(std::cmp::min(backoff, remaining));
                } else {
                    std::thread::sleep(backoff);
                }
                backoff = std::cmp::min(backoff * 2, max_backoff);
            }
            CursorResult::FellBehind => {
                header = pool.header_from_mmap()?;
                if cfg.tail > 0 {
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
    use super::{Error, ErrorKind, error_text, parse_duration, parse_size};
    use serde_json::json;
    use std::io::Cursor;
    use std::time::Duration;

    fn read_json_stream<R, F>(reader: R, mut on_value: F) -> Result<usize, Error>
    where
        R: std::io::Read,
        F: FnMut(serde_json::Value) -> Result<(), Error>,
    {
        let stream = serde_json::Deserializer::from_reader(reader).into_iter::<serde_json::Value>();
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
    fn parse_duration_accepts_ms_s_m() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("5s").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
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
