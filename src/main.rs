//! Purpose: `plasmite` CLI entry point and v0.0.1 command dispatch.
//! Role: Binary crate root; parses args, runs commands, emits JSON on stdout.
//! Invariants: Commands emit stable stdout formats (human or JSON by command/flags).
//! Invariants: Non-interactive errors are emitted as JSON on stderr.
//! Invariants: Process exit code is derived from `api::to_exit_code`.
//! Invariants: All pool mutations go through `api::Pool` (locks + mmap safety).
#![allow(clippy::result_large_err)]
use std::ffi::OsString;
use std::io::{self, IsTerminal, Read};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use clap::{
    Args, CommandFactory, Parser, Subcommand, ValueEnum, ValueHint,
    error::ErrorKind as ClapErrorKind,
};
use clap_complete::aot::Shell;
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::error::Error as StdError;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

mod color_json;
mod command_dispatch;
mod ingest;
mod jq_filter;
mod pool_info_json;
mod pool_paths;
mod serve;
mod serve_init;

use color_json::colorize_json;
use ingest::{ErrorPolicy, IngestConfig, IngestFailure, IngestMode, IngestOutcome, ingest};
use jq_filter::{JqFilter, compile_filters, matches_all};
use plasmite::api::{
    AppendOptions, Cursor, CursorResult, Durability, Error, ErrorKind, FrameRef, Lite3DocRef,
    LocalClient, Pool, PoolOptions, PoolRef, RemoteClient, RemotePool, TailOptions,
    ValidationIssue, ValidationReport, ValidationStatus, lite3,
    notify::{self, NotifyWait},
    to_exit_code,
};
use plasmite::notice::{Notice, notice_json};
use pool_info_json::{bounds_json, pool_info_json};
use pool_paths::{PoolNameResolveError, default_pool_dir, resolve_named_pool_path};

#[derive(Copy, Clone, Debug)]
struct RunOutcome {
    exit_code: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PoolTarget {
    LocalPath(PathBuf),
    Remote { base_url: String, pool: String },
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
            ClapErrorKind::DisplayHelp
            | ClapErrorKind::DisplayVersion
            | ClapErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                err.print().map_err(|io_err| {
                    (
                        Error::new(ErrorKind::Io)
                            .with_message("failed to write help")
                            .with_source(io_err),
                        ColorMode::Auto,
                    )
                })?;
                let exit_code = if matches!(
                    err.kind(),
                    ClapErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                ) {
                    2
                } else {
                    0
                };
                return Ok(RunOutcome::with_code(exit_code));
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

    let result = command_dispatch::dispatch_command(cli.command, pool_dir, color_mode);

    result
        .map_err(add_corrupt_hint)
        .map_err(add_io_hint)
        .map_err(add_internal_hint)
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
  - `feed` sends messages (write)
  - `follow` follows messages (read/stream)
  - `fetch` fetches one message by seq
"#,
    after_help = r#"EXAMPLES
  $ plasmite pool create chat
  $ plasmite follow chat              # Terminal 1: bob follows (waits for messages)
  $ plasmite feed chat '{"from": "alice", "msg": "hello"}'   # Terminal 2: alice sends
  # bob sees: {"seq":1,"time":"...","meta":{"tags":[]},"data":{"from":"alice","msg":"hello"}}

LEARN MORE
  Common pool operations:
    plasmite pool create <name>
    plasmite pool info <name>
    plasmite pool list
    plasmite pool delete <name>...

  $ plasmite <command> --help
  https://github.com/sandover/plasmite"#,
    arg_required_else_help = true,
    disable_help_subcommand = false
)]
struct Cli {
    #[arg(
        long,
        help = "Pool directory for named pools (default: ~/.plasmite/pools)",
        value_hint = ValueHint::DirPath
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
enum FollowFormat {
    Pretty,
    Jsonl,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
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
  $ plasmite pool delete foo bar baz

NOTES
  - Default location: ~/.plasmite/pools (override with --dir)"#
    )]
    Pool {
        #[command(subcommand)]
        command: PoolCommand,
    },
    #[command(
        arg_required_else_help = true,
        about = "Send a message to a pool",
        long_about = r#"Send JSON messages to a pool.

Accepts local pool refs (name/path), remote shorthand refs (http(s)://host:port/<pool>),
inline JSON, file input (-f/--file), or streams via stdin (auto-detected)."#,
        after_help = r#"EXAMPLES
  $ plasmite feed foo '{"hello": "world"}'                      # inline JSON
  $ plasmite feed foo --tag sev1 '{"msg": "alert"}'             # with tags
  $ jq -c '.[]' data.json | plasmite feed foo                   # stream from pipe"#,
        after_long_help = r#"EXAMPLES
  # Inline JSON
  $ plasmite feed foo '{"hello": "world"}'

  # Tag messages with --tag
  $ plasmite feed foo --tag ping --tag from-alice '{"msg": "hello bob"}'

  # Pipe JSON Lines
  $ jq -c '.items[]' data.json | plasmite feed foo

  # Replay a JSONL file
  $ plasmite feed foo -f events.jsonl

  # Stream from curl (event streams auto-detected)
  $ curl -N https://api.example.com/events | plasmite feed events

  # Remote shorthand ref (serve must already expose the pool)
  $ plasmite feed http://127.0.0.1:9700/demo --tag remote '{"msg":"hello"}'

  # Auto-create pool on first feed
  $ plasmite feed bar --create '{"first": "message"}'

NOTES
  - Remote refs must be shorthand: http(s)://host:port/<pool> (no trailing slash)
  - API-shaped URLs (e.g. /v0/pools/<pool>/append) are rejected as POOL refs
  - `--create` is local-only; remote feed never creates remote pools
  - `--in auto` detects JSONL, JSON-seq (0x1e), event streams (data: prefix)
  - `--errors skip` continues past bad records; `--durability flush` syncs to disk
  - `--retry N` retries on transient failures (lock contention, etc.)"#
    )]
    Feed {
        #[arg(help = "Pool ref: local name/path or shorthand URL http(s)://host:port/<pool>")]
        pool: String,
        #[arg(help = "Inline JSON value")]
        data: Option<String>,
        #[arg(long, help = "Repeatable tag for the message")]
        tag: Vec<String>,
        #[arg(
            short = 'f',
            long = "file",
            help = "Input file path (JSON value or stream; use - for stdin)",
            conflicts_with = "data",
            value_hint = ValueHint::FilePath
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
            help = "Input mode for stdin streams",
            long_help = r#"Input mode for stdin streams

  auto   Detect from stream prefix (JSONL, JSON-seq 0x1e, SSE data:)
  jsonl  One JSON object per line
  json   Single JSON value (object or array)
  seq    RFC 7464 JSON Text Sequences (0x1e-delimited)
  jq     jq --raw-output / --stream output"#
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
        #[arg(
            long,
            help = "Bearer token for remote refs (dev-only; prefer --token-file)",
            help_heading = "Remote auth/TLS"
        )]
        token: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Read bearer token from file for remote refs",
            value_hint = ValueHint::FilePath,
            help_heading = "Remote auth/TLS"
        )]
        token_file: Option<PathBuf>,
        #[arg(
            long = "tls-ca",
            value_name = "PATH",
            help = "Trust this PEM CA/certificate for remote TLS",
            value_hint = ValueHint::FilePath,
            help_heading = "Remote auth/TLS"
        )]
        tls_ca: Option<PathBuf>,
        #[arg(
            long = "tls-skip-verify",
            help = "Disable remote TLS certificate verification (unsafe; dev-only)",
            help_heading = "Remote auth/TLS"
        )]
        tls_skip_verify: bool,
    },
    #[command(
        about = "Serve pools over HTTP (loopback default in v0)",
        long_about = r#"Serve pools over HTTP (loopback default in v0).

Implements the remote protocol spec under spec/remote/v0/SPEC.md."#,
        after_help = r#"EXAMPLES
  $ plasmite serve                                              # loopback, no auth
  $ plasmite serve init                                         # bootstrap TLS + token
  $ plasmite serve check                                        # validate config"#,
        after_long_help = r#"EXAMPLES
  $ plasmite serve
  $ plasmite serve --bind 127.0.0.1:9701 --token devtoken
  $ plasmite serve --token-file /path/to/token
  $ plasmite serve --tls-self-signed
  $ plasmite serve check
  $ plasmite serve init --output-dir ./.plasmite-serve

NOTES
  - `plasmite serve` prints a startup "next commands" block on interactive terminals
  - Use `plasmite serve check` to validate config and inspect resolved endpoints without binding sockets
  - Use `plasmite serve init` to scaffold token + TLS artifacts for safer non-loopback setup
  - Loopback is the default; non-loopback binds require --allow-non-loopback
  - Use Authorization: Bearer <token> when --token or --token-file is set
  - Prefer --token-file for non-loopback deployments; --token is dev-only
  - Use --access to restrict read/write operations
  - Non-loopback writes require TLS + --token-file (or --insecure-no-tls for demos)
  - --tls-self-signed is for demos; clients must trust the generated cert
  - Use repeatable --cors-origin to allow browser clients from specific origins
  - Safety limits: --max-body-bytes, --max-tail-timeout-ms, --max-tail-concurrency"#
    )]
    Serve {
        #[command(subcommand)]
        subcommand: Option<ServeSubcommand>,
        #[command(flatten)]
        run: ServeRunArgs,
    },
    #[command(
        arg_required_else_help = true,
        about = "Fetch one message by sequence number",
        long_about = r#"Fetch a specific message by its seq number and print as JSON."#,
        after_help = r#"EXAMPLES
  $ plasmite fetch foo 1
  $ plasmite fetch foo 42 | jq '.data'"#
    )]
    Fetch {
        #[arg(help = "Pool name or path")]
        pool: String,
        #[arg(help = "Sequence number")]
        seq: u64,
    },
    #[command(
        arg_required_else_help = true,
        about = "Follow messages from a pool",
        long_about = r#"Follow a pool and stream messages as they arrive.

By default, `follow` waits for new messages forever (Ctrl-C to stop).
Use `--tail N` to see recent history first, then keep following.
Use `--replay N` with `--tail` or `--since` to replay with timing."#,
        after_help = r#"EXAMPLES
  $ plasmite follow foo                                           # follow live
  $ plasmite follow foo --tail 10                                 # last 10 + live
  $ plasmite follow foo --where '.data.ok == true' --one          # match & exit
  $ plasmite follow foo --format jsonl | jq '.data'               # pipe to jq"#,
        after_long_help = r#"EXAMPLES
  # Follow for new messages
  $ plasmite follow foo

  # Last 10 messages, then keep following
  $ plasmite follow foo --tail 10

  # Emit one matching message, then exit
  $ plasmite follow foo --where '.data.status == "error"' --one

  # Messages from the last 5 minutes
  $ plasmite follow foo --since 5m

  # Replay at original timing (or 2x, 0.5x, 0 = instant)
  $ plasmite follow foo --tail 100 --replay 1

  # Filter by exact tag (repeat for AND)
  $ plasmite follow foo --tag ping --one

  # Pipe to jq
  $ plasmite follow foo --format jsonl | jq -r '.data.msg'

  # Wait up to 5 seconds for a message
  $ plasmite follow foo --timeout 5s

  # Remote shorthand ref (serve must already expose the pool)
  $ plasmite follow http://127.0.0.1:9700/demo --tail 20 --format jsonl

NOTES
  - Use `--format jsonl` for scripts (one JSON object per line)
  - `--tag` matches exact tags; `--where` uses jq-style expressions; repeat either for AND
  - `--since 5m` and `--since 2026-01-15T10:00:00Z` both work
  - Remote refs must be shorthand: http(s)://host:port/<pool> (no trailing slash)
  - Remote `follow` supports `--tail`, `--tag`, `--where`, `--one`, `--timeout`, `--data-only`, and `--format`
  - `--create` is local-only; remote follow never creates remote pools
  - `--replay N` exits when all selected messages are emitted (no live follow); `--replay 0` emits instantly"#
    )]
    Follow {
        #[arg(help = "Pool ref: local name/path or shorthand URL http(s)://host:port/<pool>")]
        pool: String,
        #[arg(long, help = "Create local pool if missing before following")]
        create: bool,
        #[arg(
            long = "tail",
            short = 'n',
            default_value_t = 0,
            help = "Print the last N messages first, then keep following"
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
        format: Option<FollowFormat>,
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
        #[arg(
            long = "tag",
            value_name = "TAG",
            help = "Filter messages by exact tag (repeatable; AND across repeats)"
        )]
        tags: Vec<String>,
        #[arg(long = "quiet-drops", help = "Suppress drop notices on stderr")]
        quiet_drops: bool,
        #[arg(long = "no-notify", help = "Disable semaphore wakeups (poll only)")]
        no_notify: bool,
        #[arg(
            long = "replay",
            value_name = "SPEED",
            help = "Replay with timing (1 = realtime, 2 = 2x, 0.5 = half; 0 = no delay). Requires --tail or --since"
        )]
        replay: Option<f64>,
        #[arg(
            long,
            help = "Bearer token for remote refs (dev-only; prefer --token-file)",
            help_heading = "Remote auth/TLS"
        )]
        token: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Read bearer token from file for remote refs",
            value_hint = ValueHint::FilePath,
            help_heading = "Remote auth/TLS"
        )]
        token_file: Option<PathBuf>,
        #[arg(
            long = "tls-ca",
            value_name = "PATH",
            help = "Trust this PEM CA/certificate for remote TLS",
            value_hint = ValueHint::FilePath,
            help_heading = "Remote auth/TLS"
        )]
        tls_ca: Option<PathBuf>,
        #[arg(
            long = "tls-skip-verify",
            help = "Disable remote TLS certificate verification (unsafe; dev-only)",
            help_heading = "Remote auth/TLS"
        )]
        tls_skip_verify: bool,
    },
    #[command(
        arg_required_else_help = true,
        about = "Send and follow from one command",
        long_about = r#"Read and write a pool from one process.

`duplex` follows a pool on stdout (like `follow`) while also sending input from stdin:

- TTY stdin: requires `--me`; each non-empty line appends a message with `.data = {"from": ME, "msg": LINE}`.
  Your own messages are hidden from output unless `--echo-self` is set.
- Non-TTY stdin: ingests stdin as a JSON stream (like `feed`, defaults: `--in auto --errors stop`).
  Duplex exits when stdin ends (EOF) or when the receive side ends (e.g. timeout/error).

Notes:
- Remote refs do not support `--create` or `--since` (use `--tail` for remote)."#
    )]
    Duplex {
        #[arg(help = "Pool ref: local name/path or shorthand URL http(s)://host:port/<pool>")]
        pool: String,
        #[arg(
            long,
            help = "Sender identity for TTY mode and default self-suppression"
        )]
        me: Option<String>,
        #[arg(long, help = "Create local pool if missing before following")]
        create: bool,
        #[arg(
            long = "tail",
            short = 'n',
            default_value_t = 0,
            help = "Print the last N messages first"
        )]
        tail: u64,
        #[arg(long, help = "Emit JSON Lines (one object per line)")]
        jsonl: bool,
        #[arg(
            long,
            help = "Exit 124 if no output within duration (e.g. 500ms, 5s, 1m)"
        )]
        timeout: Option<String>,
        #[arg(
            long = "format",
            value_enum,
            help = "Output format: pretty|jsonl (use --jsonl as alias for jsonl)"
        )]
        format: Option<FollowFormat>,
        #[arg(
            long,
            help = "Start at or after this time (RFC 3339 or relative like 5m)",
            conflicts_with = "tail"
        )]
        since: Option<String>,
        #[arg(long, help = "Also emit your own messages in the receive stream")]
        echo_self: bool,
    },
    #[command(
        arg_required_else_help = true,
        about = "Diagnose pool health",
        long_about = r#"Validate one pool (or all pools) and emit a diagnostic report."#,
        after_help = r#"EXAMPLES
  $ plasmite doctor foo
  $ plasmite doctor --all
  $ plasmite doctor --all --json

NOTES
  - Human-readable output is the default.
  - Use --json for machine-readable output.
  - Exits nonzero when corruption is detected."#
    )]
    Doctor {
        #[arg(help = "Pool name or path", required = false)]
        pool: Option<String>,
        #[arg(long, help = "Validate all pools in the pool directory")]
        all: bool,
        #[arg(long, help = "Emit JSON instead of human-readable output")]
        json: bool,
    },
    #[command(
        about = "Print version info as JSON",
        long_about = r#"Emit version info as JSON (stable, machine-readable)."#,
        after_help = r#"EXAMPLES
  $ plasmite version"#
    )]
    Version,
    #[command(
        arg_required_else_help = true,
        about = "Generate shell completions",
        long_about = r#"Generate shell completion scripts.

Prints a completion script for the given shell to stdout.
Install the generated file in your shell's completion directory (or source it)
to enable tab completion."#,
        after_help = r#"EXAMPLES
  $ plasmite completion bash > ~/.local/share/bash-completion/completions/plasmite
  $ source ~/.bashrc
  $ plasmite completion zsh > ~/.zfunc/_plasmite
  $ autoload -U compinit && compinit
  $ plasmite completion fish > ~/.config/fish/completions/plasmite.fish"#
    )]
    Completion {
        #[arg(help = "Shell to generate completions for")]
        shell: Shell,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum AccessModeCli {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

impl From<AccessModeCli> for serve::AccessMode {
    fn from(value: AccessModeCli) -> Self {
        match value {
            AccessModeCli::ReadOnly => serve::AccessMode::ReadOnly,
            AccessModeCli::WriteOnly => serve::AccessMode::WriteOnly,
            AccessModeCli::ReadWrite => serve::AccessMode::ReadWrite,
        }
    }
}

#[derive(Subcommand)]
enum PoolCommand {
    #[command(
        arg_required_else_help = true,
        about = "Create one or more pools",
        long_about = r#"Create pool files. Default size is 1MB (use --size for larger).

Pools include an inline sequence index by default for fast `get(seq)` lookups."#,
        after_help = r#"EXAMPLES
  $ plasmite pool create foo
  $ plasmite pool create --size 8M bar baz quux
  $ plasmite pool create --size 8M --index-capacity 4096 indexed
  $ plasmite pool create --json foo

NOTES
  - Sizes: 64K, 1M, 8M, 1G (K/M/G are 1024-based)"#
    )]
    Create {
        #[arg(required = true, help = "Pool name(s) to create")]
        names: Vec<String>,
        #[arg(long, help = "Pool size (bytes or K/M/G)")]
        size: Option<String>,
        #[arg(
            long = "index-capacity",
            help = "Inline index slot count (default: auto-size; 0 disables index)"
        )]
        index_capacity: Option<u32>,
        #[arg(long, help = "Emit JSON instead of human-readable output")]
        json: bool,
    },
    #[command(
        arg_required_else_help = true,
        about = "Show pool metadata and bounds",
        long_about = r#"Show pool size, bounds, and metrics in human-readable format by default."#,
        after_help = r#"EXAMPLES
  $ plasmite pool info foo
  $ plasmite pool info foo --json"#
    )]
    Info {
        #[arg(help = "Pool name or path")]
        name: String,
        #[arg(long, help = "Emit JSON instead of human-readable output")]
        json: bool,
    },
    #[command(
        arg_required_else_help = true,
        about = "Delete one or more pool files",
        long_about = r#"Delete one or more pool files (destructive, cannot be undone)."#,
        after_help = r#"EXAMPLES
  $ plasmite pool delete foo
  $ plasmite pool delete foo bar baz
  $ plasmite pool delete --json foo bar

NOTES
  - Human-readable output is the default.
  - Use --json for machine-readable output.
  - Best effort: attempts all deletes and reports per-pool failures.
  - Exits non-zero if any requested pool failed to delete."#
    )]
    Delete {
        #[arg(required = true, help = "Pool name(s) or path(s)")]
        names: Vec<String>,
        #[arg(long, help = "Emit JSON instead of human-readable output")]
        json: bool,
    },
    #[command(
        about = "List pools in the pool directory",
        long_about = r#"List pools in the pool directory.

Prints a human-readable table by default. Use --json for machine-readable output."#,
        after_help = r#"EXAMPLES
  $ plasmite pool list
  $ plasmite pool list --json

NOTES
  - Human-readable output is the default.
  - Use --json for machine-readable output.
  - Non-.plasmite files are ignored.
  - Pools that cannot be read include an error field."#
    )]
    List {
        #[arg(long, help = "Emit JSON instead of human-readable output")]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ServeSubcommand {
    #[command(
        about = "Bootstrap secure serve token/TLS artifacts",
        long_about = r#"Generate token + TLS artifacts and print copy/paste next commands for secure serve startup."#,
        after_help = r#"EXAMPLES
  $ plasmite serve init
  $ plasmite serve init --output-dir ./.plasmite-serve
  $ plasmite serve init --output-dir ./.plasmite-serve --force

NOTES
  - Writes token/cert/key files without printing secret token values
  - Refuses to overwrite existing artifacts unless --force is set"#
    )]
    Init(ServeInitArgs),
    #[command(
        about = "Validate serve config and print effective endpoints without starting",
        long_about = r#"Validate serve config and print effective endpoints without starting a server."#,
        after_help = r#"EXAMPLES
  $ plasmite serve check
  $ plasmite serve --bind 0.0.0.0:9700 --allow-non-loopback --access read-only check
  $ plasmite serve --token-file ~/.plasmite/token --tls-self-signed check

NOTES
  - Exits non-zero when config is invalid
  - Does not bind sockets or start background tasks
  - Human-readable output is the default; use --json for machine output"#
    )]
    Check {
        #[arg(long, help = "Emit JSON instead of human-readable output")]
        json: bool,
    },
}

#[derive(Args)]
struct ServeInitArgs {
    #[arg(
        long,
        default_value = "0.0.0.0:9700",
        help = "Bind address used in printed next commands"
    )]
    bind: String,
    #[arg(
        long,
        default_value = ".",
        value_name = "PATH",
        help = "Base output directory for generated artifacts",
        value_hint = ValueHint::DirPath
    )]
    output_dir: PathBuf,
    #[arg(
        long,
        default_value = "serve-token.txt",
        value_name = "PATH",
        help = "Token output path (relative to --output-dir unless absolute)",
        value_hint = ValueHint::FilePath
    )]
    token_file: PathBuf,
    #[arg(
        long = "tls-cert",
        default_value = "serve-cert.pem",
        value_name = "PATH",
        help = "TLS certificate output path (relative to --output-dir unless absolute)",
        value_hint = ValueHint::FilePath
    )]
    tls_cert: PathBuf,
    #[arg(
        long = "tls-key",
        default_value = "serve-key.pem",
        value_name = "PATH",
        help = "TLS private key output path (relative to --output-dir unless absolute)",
        value_hint = ValueHint::FilePath
    )]
    tls_key: PathBuf,
    #[arg(long, help = "Overwrite existing generated artifacts")]
    force: bool,
}

#[derive(Args)]
struct ServeRunArgs {
    #[arg(
        long,
        default_value = "127.0.0.1:9700",
        help = "Bind address",
        help_heading = "Connection"
    )]
    bind: String,
    #[arg(
        long,
        value_enum,
        default_value = "read-write",
        help = "Access mode: read-only|write-only|read-write",
        help_heading = "Connection"
    )]
    access: AccessModeCli,
    #[arg(
        long = "cors-origin",
        value_name = "ORIGIN",
        help = "Allow browser requests from this origin (repeatable, explicit list)",
        help_heading = "Connection"
    )]
    cors_origin: Vec<String>,
    #[arg(
        long,
        help = "Bearer token for auth (dev-only; prefer --token-file)",
        help_heading = "Authentication"
    )]
    token: Option<String>,
    #[arg(long, value_name = "PATH", help = "Read bearer token from file", value_hint = ValueHint::FilePath, help_heading = "Authentication")]
    token_file: Option<PathBuf>,
    #[arg(long, value_name = "PATH", help = "TLS certificate path (PEM)", value_hint = ValueHint::FilePath, help_heading = "TLS")]
    tls_cert: Option<PathBuf>,
    #[arg(long, value_name = "PATH", help = "TLS key path (PEM)", value_hint = ValueHint::FilePath, help_heading = "TLS")]
    tls_key: Option<PathBuf>,
    #[arg(
        long,
        help = "Generate a self-signed TLS cert for this run",
        help_heading = "TLS"
    )]
    tls_self_signed: bool,
    #[arg(
        long,
        help = "Allow non-loopback binds (unsafe without TLS + token)",
        help_heading = "Safety"
    )]
    allow_non_loopback: bool,
    #[arg(
        long,
        help = "Allow non-loopback writes without TLS (unsafe)",
        help_heading = "Safety"
    )]
    insecure_no_tls: bool,
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_BODY_BYTES,
        help = "Max request body size in bytes",
        help_heading = "Safety"
    )]
    max_body_bytes: u64,
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_TAIL_TIMEOUT_MS,
        help = "Max tail timeout in milliseconds",
        help_heading = "Safety"
    )]
    max_tail_timeout_ms: u64,
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_TAIL_CONCURRENCY,
        help = "Max concurrent tail streams",
        help_heading = "Safety"
    )]
    max_tail_concurrency: usize,
}

fn resolve_poolref(input: &str, pool_dir: &Path) -> Result<PathBuf, Error> {
    if input.chars().any(std::path::is_separator) {
        return Ok(PathBuf::from(input));
    }
    resolve_named_pool_path(input, pool_dir).map_err(map_pool_name_resolve_error)
}

fn map_pool_name_resolve_error(err: PoolNameResolveError) -> Error {
    match err {
        PoolNameResolveError::ContainsPathSeparator => {
            Error::new(ErrorKind::Usage).with_message("pool name must not contain path separators")
        }
    }
}

fn resolve_pool_target(input: &str, pool_dir: &Path) -> Result<PoolTarget, Error> {
    if input.starts_with("http://") || input.starts_with("https://") {
        return parse_remote_pool_target(input);
    }
    if input.contains("://") {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote pool ref must use http or https scheme")
            .with_hint("Use shorthand: http(s)://host:port/<pool>."));
    }
    resolve_poolref(input, pool_dir).map(PoolTarget::LocalPath)
}

fn parse_remote_pool_target(input: &str) -> Result<PoolTarget, Error> {
    let mut url = Url::parse(input).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid remote pool ref")
            .with_hint("Use shorthand: http(s)://host:port/<pool>.")
            .with_source(err)
    })?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote pool ref must not include query or fragment")
            .with_hint("Use shorthand: http(s)://host:port/<pool>."));
    }
    let path = url.path();
    if path.contains("%2f") || path.contains("%2F") {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote pool name must not contain path separators")
            .with_hint("Use a single pool segment: http(s)://host:port/<pool>."));
    }
    let segments: Vec<_> = url
        .path_segments()
        .map(|parts| parts.collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.len() != 1
        || segments[0].is_empty()
        || segments[0] == "pool"
        || (segments.len() >= 2 && segments[0] == "pools")
        || (segments.len() >= 3 && segments[0] == "v0" && segments[1] == "pools")
    {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote pool ref must use shorthand http(s)://host:port/<pool>")
            .with_hint("API-shaped URLs are not accepted as pool refs."));
    }
    let pool = segments[0].to_string();
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(PoolTarget::Remote {
        base_url: url.to_string(),
        pool,
    })
}

const DEFAULT_POOL_SIZE: u64 = 1024 * 1024;
const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(50);
const DEFAULT_SNIFF_BYTES: usize = 8 * 1024;
const DEFAULT_SNIFF_LINES: usize = 8;
const DEFAULT_MAX_RECORD_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_SNIPPET_BYTES: usize = 200;
const DEFAULT_MAX_BODY_BYTES: u64 = 1024 * 1024;
const DEFAULT_MAX_TAIL_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_TAIL_CONCURRENCY: usize = 64;

// ── Missing-pool remediation hint policy ──────────────────────────────────
//
// When a pool is not found, the CLI tries to suggest a retry command with
// `--create`.  The rendering strategy is *shell-agnostic argv echo*:
//
//   • Render an exact command only when the CLI has a stable, unambiguous argv
//     token sequence available at error time (inline JSON, --file, repeated
//     flags, etc.).
//   • When the data source is stdin/pipe, exact reconstruction is unsafe —
//     fall back to generic wording ("add --create to your invocation").
//   • Never infer data not present in argv context.
//   • Tokens that contain special characters are JSON-escaped rather than
//     shell-quoted, keeping the hint correct across bash/zsh/fish/PowerShell.
//
// Coverage checklist (each shape should have a matching integration test):
//   1. Inline JSON payload         → exact command emitted
//   2. Paths with spaces (--file)  → exact command with quoted path args
//   3. Repeated flags (--tag …)    → exact command preserves repeated flags
//   4. Stdin/pipe usage            → fallback wording (no exact command)
//
// See also: `render_shell_agnostic_token`, `render_shell_agnostic_command`,
//           `feed_exact_create_command_hint`, `follow_exact_create_command_hint`.
// ──────────────────────────────────────────────────────────────────────────

fn add_missing_pool_hint(err: Error, pool_ref: &str, input: &str) -> Error {
    if err.kind() != ErrorKind::NotFound || err.hint().is_some() {
        return err;
    }
    if input.chars().any(std::path::is_separator) {
        return err.with_hint(
            "Pool path not found. Check the path or pass --dir for a different pool directory.",
        );
    }
    err.with_hint(format!(
        "Create it first: plasmite pool create {pool_ref} (or pass --dir for a different pool directory)."
    ))
}

fn add_missing_pool_create_hint(
    err: Error,
    command: &str,
    pool_ref: &str,
    input: &str,
    exact_command: Option<String>,
) -> Error {
    if err.kind() != ErrorKind::NotFound || err.hint().is_some() {
        return err;
    }
    if input.contains("://") {
        return err.with_hint("Remote pool not found. Create it with server-side tooling first.");
    }
    if input.chars().any(std::path::is_separator) {
        return err.with_hint(
            "Pool path not found. Check the path or pass --dir for a different pool directory.",
        );
    }
    if let Some(exact_command) = exact_command {
        return err.with_hint(format!(
            "Pool is missing. Retry with exact command: {exact_command}"
        ));
    }
    err.with_hint(format!(
        "Pool is missing. Re-run with --create (local refs only), e.g. plasmite {command} {pool_ref} --create."
    ))
}

fn render_shell_agnostic_token(token: &str) -> String {
    if !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '='))
    {
        token.to_string()
    } else {
        serde_json::to_string(token).unwrap_or_else(|_| format!("\"{token}\""))
    }
}

fn render_shell_agnostic_command(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| render_shell_agnostic_token(token))
        .collect::<Vec<_>>()
        .join(" ")
}

struct FeedExactCreateHint<'a> {
    tags: &'a [String],
    data: &'a Option<String>,
    file: &'a Option<String>,
    durability: Durability,
    retry: u32,
    retry_delay: Option<&'a str>,
    input: InputMode,
    errors: ErrorPolicyCli,
    single_input: bool,
}

fn feed_exact_create_command_hint(pool: &str, options: FeedExactCreateHint<'_>) -> Option<String> {
    if !options.single_input {
        return None;
    }
    let mut tokens = vec![
        "plasmite".to_string(),
        "feed".to_string(),
        pool.to_string(),
        "--create".to_string(),
    ];
    for tag in options.tags {
        tokens.push("--tag".to_string());
        tokens.push(tag.clone());
    }
    if let Some(data) = options.data {
        tokens.push(data.clone());
    }
    if let Some(file) = options.file {
        tokens.push("--file".to_string());
        tokens.push(file.clone());
    }
    if options.durability != Durability::Fast {
        tokens.push("--durability".to_string());
        tokens.push(
            match options.durability {
                Durability::Fast => "fast",
                Durability::Flush => "flush",
            }
            .to_string(),
        );
    }
    if options.retry > 0 {
        tokens.push("--retry".to_string());
        tokens.push(options.retry.to_string());
    }
    if let Some(delay) = options.retry_delay {
        tokens.push("--retry-delay".to_string());
        tokens.push(delay.to_string());
    }
    if options.input != InputMode::Auto {
        tokens.push("--in".to_string());
        tokens.push(
            match options.input {
                InputMode::Auto => "auto",
                InputMode::Jsonl => "jsonl",
                InputMode::Json => "json",
                InputMode::Seq => "seq",
                InputMode::Jq => "jq",
            }
            .to_string(),
        );
    }
    if options.errors != ErrorPolicyCli::Stop {
        tokens.push("--errors".to_string());
        tokens.push("skip".to_string());
    }
    Some(render_shell_agnostic_command(&tokens))
}

#[allow(clippy::too_many_arguments)]
fn follow_exact_create_command_hint(
    pool: &str,
    tail: u64,
    one: bool,
    jsonl: bool,
    timeout: Option<&str>,
    data_only: bool,
    format: Option<FollowFormat>,
    since: Option<&str>,
    where_expr: &[String],
    tags: &[String],
    quiet_drops: bool,
    no_notify: bool,
    replay: Option<f64>,
) -> String {
    let mut tokens = vec![
        "plasmite".to_string(),
        "follow".to_string(),
        pool.to_string(),
        "--create".to_string(),
    ];
    if tail > 0 {
        tokens.push("--tail".to_string());
        tokens.push(tail.to_string());
    }
    if one {
        tokens.push("--one".to_string());
    }
    if jsonl {
        tokens.push("--jsonl".to_string());
    }
    if let Some(timeout) = timeout {
        tokens.push("--timeout".to_string());
        tokens.push(timeout.to_string());
    }
    if data_only {
        tokens.push("--data-only".to_string());
    }
    if let Some(format) = format {
        tokens.push("--format".to_string());
        tokens.push(
            match format {
                FollowFormat::Pretty => "pretty",
                FollowFormat::Jsonl => "jsonl",
            }
            .to_string(),
        );
    }
    if let Some(since) = since {
        tokens.push("--since".to_string());
        tokens.push(since.to_string());
    }
    for expr in where_expr {
        tokens.push("--where".to_string());
        tokens.push(expr.clone());
    }
    for tag in tags {
        tokens.push("--tag".to_string());
        tokens.push(tag.clone());
    }
    if quiet_drops {
        tokens.push("--quiet-drops".to_string());
    }
    if no_notify {
        tokens.push("--no-notify".to_string());
    }
    if let Some(replay) = replay {
        tokens.push("--replay".to_string());
        tokens.push(replay.to_string());
    }
    render_shell_agnostic_command(&tokens)
}

fn add_missing_seq_hint(err: Error, pool_ref: &str) -> Error {
    if err.kind() != ErrorKind::NotFound || err.seq().is_none() || err.hint().is_some() {
        return err;
    }
    err.with_hint(format!(
        "Check available messages: plasmite pool info {pool_ref} (or plasmite follow {pool_ref} --tail 10)."
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

fn add_internal_hint(err: Error) -> Error {
    if err.kind() != ErrorKind::Internal || err.hint().is_some() {
        return err;
    }
    err.with_hint(
        "Unexpected internal failure. Retry with RUST_BACKTRACE=1 and share command/context if it persists.",
    )
}

fn emit_doctor_human(report: &ValidationReport) {
    if !io::stdout().is_terminal() {
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
        return;
    }

    let label = doctor_display_label(report);
    match report.status {
        ValidationStatus::Ok => {
            println!("{label}: healthy");
            println!("  messages:  {}", doctor_messages_summary(report));
            println!("  checked:   header, index, ring — 0 issues");
        }
        ValidationStatus::Corrupt => {
            let issue = report
                .issues
                .first()
                .map(|value| value.message.clone())
                .unwrap_or_else(|| "corruption detected".to_string());
            println!("{label}: corrupt");
            println!("  messages:  {}", doctor_messages_summary(report));
            println!(
                "  checked:   header, index, ring — {} issues",
                report.issues.len()
            );
            println!("  detail:    {issue}");
        }
    }
}

fn emit_doctor_human_summary(reports: &[ValidationReport]) {
    if reports.is_empty() {
        println!("No pools found.");
        return;
    }
    if !io::stdout().is_terminal() {
        for report in reports {
            emit_doctor_human(report);
        }
        return;
    }

    let corrupt = reports
        .iter()
        .filter(|report| report.status == ValidationStatus::Corrupt)
        .count();
    let labels = reports.iter().map(doctor_display_label).collect::<Vec<_>>();
    let message_labels = reports
        .iter()
        .map(doctor_messages_count_label)
        .collect::<Vec<_>>();
    let label_width = labels.iter().map(|value| value.len()).max().unwrap_or(0);
    let message_width = message_labels
        .iter()
        .map(|value| value.len())
        .max()
        .unwrap_or(0);
    if corrupt == 0 {
        println!("All {} pools healthy.", reports.len());
        println!();
        for idx in 0..reports.len() {
            println!(
                "  {:<label_width$}   {:<message_width$}   0 issues",
                labels[idx], message_labels[idx]
            );
        }
    } else {
        println!("{corrupt} of {} pools unhealthy.", reports.len());
        println!();
        for (idx, report) in reports.iter().enumerate() {
            let label = &labels[idx];
            let messages = &message_labels[idx];
            if report.status == ValidationStatus::Corrupt {
                println!(
                    "  ✗ {:<label_width$}   {:<message_width$}   {} issues (run `pls doctor {}` for detail)",
                    label,
                    messages,
                    report.issues.len(),
                    label
                );
            } else {
                println!("  ✓ {label:<label_width$}   {messages:<message_width$}   0 issues");
            }
        }
    }
}

fn doctor_display_label(report: &ValidationReport) -> String {
    if let Some(pool_ref) = report.pool_ref.as_deref() {
        let looks_like_path = pool_ref.contains('/') || pool_ref.contains('\\');
        if !looks_like_path {
            return pool_ref.to_string();
        }
    }
    if let Some(stem) = report.path.file_stem().and_then(|value| value.to_str()) {
        return stem.to_string();
    }
    short_display_path(&report.path, report.path.parent())
}

fn doctor_messages_summary(report: &ValidationReport) -> String {
    if let Some(stats) = doctor_message_stats(report) {
        let seq_range = format_seq_range(stats.oldest_seq, stats.newest_seq);
        if stats.count == 0 {
            return "empty".to_string();
        }
        if seq_range == "-" {
            return stats.count.to_string();
        }
        return format!("{} ({seq_range})", stats.count);
    }

    let seq_range = format_seq_range(report.last_good_seq, report.last_good_seq);
    if seq_range == "-" {
        "empty".to_string()
    } else {
        format!("visible count unavailable ({seq_range})")
    }
}

fn doctor_messages_count_label(report: &ValidationReport) -> String {
    if let Some(stats) = doctor_message_stats(report) {
        return format!("{} messages", stats.count);
    }
    if report.status == ValidationStatus::Ok {
        "messages unknown".to_string()
    } else {
        report
            .last_good_seq
            .map(|seq| format!("up to seq {seq}"))
            .unwrap_or_else(|| "messages unknown".to_string())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DoctorMessageStats {
    count: u64,
    oldest_seq: Option<u64>,
    newest_seq: Option<u64>,
}

fn doctor_message_stats(report: &ValidationReport) -> Option<DoctorMessageStats> {
    let info = LocalClient::new()
        .pool_info(&PoolRef::path(report.path.clone()))
        .ok()?;
    Some(DoctorMessageStats {
        count: message_count_from_info(&info),
        oldest_seq: info.bounds.oldest_seq,
        newest_seq: info.bounds.newest_seq,
    })
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

fn list_pool_paths(pool_dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let entries = std::fs::read_dir(pool_dir).map_err(|err| {
        let kind = match err.kind() {
            std::io::ErrorKind::NotFound => ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorKind::Permission,
            _ => ErrorKind::Io,
        };
        Error::new(kind)
            .with_message("failed to read pool directory")
            .with_path(pool_dir)
            .with_source(err)
    })?;

    let mut pools = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read pool directory entry")
                .with_path(pool_dir)
                .with_source(err)
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("plasmite") {
            pools.push(path);
        }
    }
    Ok(pools)
}

fn list_pools(pool_dir: &Path, client: &LocalClient) -> Vec<Value> {
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
        let pool_ref = PoolRef::path(path.clone());
        match client.pool_info(&pool_ref) {
            Ok(info) => {
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

fn emit_pool_list_table(pools: &[Value], pool_dir: &Path) {
    let interactive = io::stdout().is_terminal();
    if interactive && pools.is_empty() {
        println!(
            "No pools found in {}",
            display_pool_dir_for_humans(pool_dir)
        );
        println!();
        println!("  Create one: plasmite pool create <name>");
        return;
    }

    let has_errors = pools.iter().any(|pool| {
        pool.get("error")
            .and_then(|value| value.get("error"))
            .is_some()
    });
    let headers = if interactive && !has_errors {
        vec!["NAME", "SIZE", "MSGS", "MODIFIED", "PATH"]
    } else {
        vec![
            "NAME", "STATUS", "SIZE", "OLDEST", "NEWEST", "MTIME", "PATH", "DETAIL",
        ]
    };
    let rows = pools
        .iter()
        .map(|pool| {
            let name = pool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("-")
                .to_string();
            let path_value = pool
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("-");
            let display_path = if path_value == "-" {
                "-".to_string()
            } else {
                short_display_path(Path::new(path_value), Some(pool_dir))
            };

            if let Some(error) = pool.get("error").and_then(|value| value.get("error")) {
                let detail = error
                    .get("message")
                    .and_then(|value| value.as_str())
                    .or_else(|| error.get("kind").and_then(|value| value.as_str()))
                    .unwrap_or("error")
                    .to_string();
                vec![
                    name,
                    "ERR".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    display_path,
                    detail,
                ]
            } else {
                let oldest = pool
                    .get("bounds")
                    .and_then(|value| value.get("oldest"))
                    .and_then(|value| value.as_u64());
                let newest = pool
                    .get("bounds")
                    .and_then(|value| value.get("newest"))
                    .and_then(|value| value.as_u64());
                let msg_count = match (oldest, newest) {
                    (Some(a), Some(b)) => b.saturating_sub(a).saturating_add(1),
                    _ => 0,
                };
                let size = pool
                    .get("file_size")
                    .and_then(|value| value.as_u64())
                    .map(|value| {
                        if interactive {
                            format_bytes(value)
                        } else {
                            value.to_string()
                        }
                    })
                    .unwrap_or_else(|| "-".to_string());
                let oldest_str = oldest
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let newest_str = newest
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let mtime = pool
                    .get("mtime")
                    .and_then(|value| value.as_str())
                    .map(|value| {
                        if interactive {
                            format_relative_from_timestamp(value)
                        } else {
                            value.to_string()
                        }
                    })
                    .unwrap_or_else(|| "-".to_string());
                if interactive && !has_errors {
                    vec![name, size, msg_count.to_string(), mtime, display_path]
                } else {
                    vec![
                        name,
                        "OK".to_string(),
                        size,
                        oldest_str,
                        newest_str,
                        mtime,
                        display_path,
                        String::new(),
                    ]
                }
            }
        })
        .collect::<Vec<_>>();

    emit_table(&headers, &rows);
}

fn emit_pool_create_table(created: &[Value], pool_dir: &Path) {
    if io::stdout().is_terminal() {
        if created.len() == 1 {
            if let Some(pool) = created.first() {
                let name = pool
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("pool");
                let size = pool
                    .get("file_size")
                    .and_then(|value| value.as_u64())
                    .map(format_bytes)
                    .unwrap_or_else(|| "-".to_string());
                let index = pool
                    .get("index_capacity")
                    .and_then(|value| value.as_u64())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let path = pool
                    .get("path")
                    .and_then(|value| value.as_str())
                    .map(|value| short_display_path(Path::new(value), Some(pool_dir)))
                    .unwrap_or_else(|| "-".to_string());
                println!("Created {name} ({size}, {index} index slots)");
                println!("  path: {path}");
            }
            return;
        }

        let size = created
            .first()
            .and_then(|pool| pool.get("file_size"))
            .and_then(|value| value.as_u64())
            .map(format_bytes)
            .unwrap_or_else(|| "-".to_string());
        println!("Created {} pools ({} each)", created.len(), size);
        for pool in created {
            let name = pool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("pool");
            let path = pool
                .get("path")
                .and_then(|value| value.as_str())
                .map(|value| short_display_path(Path::new(value), Some(pool_dir)))
                .unwrap_or_else(|| "-".to_string());
            println!("  - {name} ({path})");
        }
        return;
    }

    let headers = ["NAME", "SIZE", "INDEX", "PATH"];
    let rows = created
        .iter()
        .map(|pool| {
            let name = pool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("-")
                .to_string();
            let size = pool
                .get("file_size")
                .and_then(|value| value.as_u64())
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            let index = pool
                .get("index_capacity")
                .and_then(|value| value.as_u64())
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            let path = pool
                .get("path")
                .and_then(|value| value.as_str())
                .map(|value| short_display_path(Path::new(value), Some(pool_dir)))
                .unwrap_or_else(|| "-".to_string());
            vec![name, size, index, path]
        })
        .collect::<Vec<_>>();
    emit_table(&headers, &rows);
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

fn format_bytes(value: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    if value < KIB {
        return value.to_string();
    }
    let (unit, suffix) = if value >= GIB {
        (GIB, "G")
    } else if value >= MIB {
        (MIB, "M")
    } else {
        (KIB, "K")
    };
    if value.is_multiple_of(unit) {
        return format!("{}{}", value / unit, suffix);
    }
    format!("{:.1}{}", (value as f64) / (unit as f64), suffix)
}

fn format_timestamp_human(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "-".to_string();
    }
    let parsed =
        time::OffsetDateTime::parse(trimmed, &time::format_description::well_known::Rfc3339);
    let Ok(parsed) = parsed else {
        return trimmed.to_string();
    };
    let parsed = parsed.to_offset(time::UtcOffset::UTC);
    let format = time::format_description::parse("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    let Ok(format) = format else {
        return trimmed.to_string();
    };
    parsed
        .format(&format)
        .unwrap_or_else(|_| trimmed.to_string())
}

fn format_relative_time(age_ms: Option<u64>) -> String {
    let Some(age_ms) = age_ms else {
        return "-".to_string();
    };
    let seconds = (age_ms / 1000).max(1);
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d ago");
    }
    format!("{}w ago", days / 7)
}

fn format_seq_range(oldest: Option<u64>, newest: Option<u64>) -> String {
    match (oldest, newest) {
        (Some(oldest), Some(newest)) => format!("seq {oldest}..{newest}"),
        _ => "-".to_string(),
    }
}

fn format_relative_from_timestamp(value: &str) -> String {
    let Ok(parsed) =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
    else {
        return "-".to_string();
    };
    let now_ns = match now_ns() {
        Ok(value) => value,
        Err(_) => return "-".to_string(),
    };
    let now = match time::OffsetDateTime::from_unix_timestamp_nanos(now_ns as i128) {
        Ok(value) => value,
        Err(_) => return "-".to_string(),
    };
    let delta = now
        .unix_timestamp_nanos()
        .saturating_sub(parsed.unix_timestamp_nanos());
    let age_ms = (delta / 1_000_000) as u64;
    format_relative_time(Some(age_ms))
}

fn ensure_pool_dir(dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dir)
        .map_err(|err| Error::new(ErrorKind::Io).with_path(dir).with_source(err))
}

fn read_token_file(path: &Path) -> Result<String, Error> {
    let raw = std::fs::read_to_string(path).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("failed to read token file")
            .with_path(path)
            .with_source(err)
    })?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("token file is empty")
            .with_path(path));
    }
    Ok(token)
}

fn resolve_token_value(
    token: Option<String>,
    token_file: Option<PathBuf>,
) -> Result<Option<String>, Error> {
    if token.is_some() && token_file.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--token cannot be combined with --token-file")
            .with_hint("Use --token-file for safer handling, or pass --token for local/dev use."));
    }
    if let Some(path) = token_file {
        return read_token_file(&path).map(Some);
    }
    Ok(token)
}

fn reject_remote_only_flags_for_local_target(
    command: &str,
    token: Option<&str>,
    token_file: Option<&Path>,
    tls_ca: Option<&Path>,
    tls_skip_verify: bool,
) -> Result<(), Error> {
    if token.is_none() && token_file.is_none() && tls_ca.is_none() && !tls_skip_verify {
        return Ok(());
    }
    Err(Error::new(ErrorKind::Usage)
        .with_message(format!(
            "{command} remote auth/TLS flags require a remote http(s) pool ref"
        ))
        .with_hint("Use --token/--token-file/--tls-ca/--tls-skip-verify only with http(s)://host:port/<pool> refs."))
}

fn emit_serve_init_human(result: &serve_init::ServeInitResult) {
    let token_path = Path::new(&result.token_file);
    let cert_path = Path::new(&result.tls_cert);
    let key_path = Path::new(&result.tls_key);
    let (output_dir, token_label, cert_label, key_label) =
        serve_init_artifact_labels(token_path, cert_path, key_path);
    let token_file = display_handoff_path_from_path(token_path);
    let tls_cert = display_handoff_path_from_path(cert_path);
    let tls_key = display_handoff_path_from_path(key_path);
    let bind = extract_bind_from_server_commands(&result.server_commands)
        .unwrap_or_else(|| "0.0.0.0:9700".to_string());
    let remote_host = detect_serve_init_remote_host(&bind);
    let remote_url_host = url_host_component(&remote_host);
    let port = bind
        .parse::<SocketAddr>()
        .map(|addr| addr.port())
        .unwrap_or(9700);
    let (headline, files_heading) = if result.overwrote_existing {
        ("Secure serving re-initialized.", "Files overwritten:")
    } else {
        ("Secure serving initialized.", "Files created:")
    };

    println!("{headline}");
    println!("Clients on your network can now read and write your pools over HTTPS.");
    println!();
    if let Some(output_dir) = output_dir {
        println!("  Output directory: {output_dir}");
        println!();
    }
    println!("  {files_heading}");
    println!("    token   {token_label}");
    println!("    cert    {cert_label}");
    println!("    key     {key_label}");
    println!();
    println!("  Fingerprint (share this with clients to verify the cert):");
    println!("    {}", result.tls_fingerprint);
    println!();
    println!("  Start serving your pools:");
    println!();
    println!("    pls serve \\");
    println!("      --bind {bind} \\");
    println!("      --allow-non-loopback \\");
    println!("      --token-file {token_file} \\");
    println!("      --tls-cert {tls_cert} \\");
    println!("      --tls-key {tls_key}");
    println!();
    println!("  From another machine, read and write pools by URL:");
    println!();
    println!("    pls feed https://{remote_url_host}:{port}/demo \\");
    println!("      --token-file {token_file} \\");
    println!("      --tls-ca {tls_cert} \\");
    println!("      '{{\"hello\":\"world\"}}'");
    println!();
    println!("    pls follow https://{remote_url_host}:{port}/demo \\");
    println!("      --token-file {token_file} \\");
    println!("      --tls-ca {tls_cert} --tail 10");
    println!();
    println!("  Or with curl:");
    println!("    TOKEN=$(cat {token_file})");
    println!("    curl -k -H \"Authorization: Bearer $TOKEN\" \\");
    println!("      https://{remote_url_host}:{port}/v0/pools/demo/tail?timeout_ms=5000");
    println!();
    println!("  The token is in the file, not printed here. Share the token");
    println!("  and fingerprint with collaborators out-of-band (e.g. paste");
    println!("  in a DM). Clients use the fingerprint to verify the cert");
    println!("  on first connect.");
}

fn detect_serve_init_remote_host(bind: &str) -> String {
    if let Ok(addr) = bind.parse::<SocketAddr>() {
        let ip = addr.ip();
        if !ip.is_unspecified() && !ip.is_loopback() {
            return ip.to_string();
        }
    }
    detect_primary_non_loopback_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "YOUR-HOST".to_string())
}

fn detect_primary_non_loopback_ip() -> Option<IpAddr> {
    detect_non_loopback_ip_via("0.0.0.0:0", "8.8.8.8:53")
        .or_else(|| detect_non_loopback_ip_via("[::]:0", "[2001:4860:4860::8888]:53"))
}

fn detect_non_loopback_ip_via(bind_addr: &str, probe_addr: &str) -> Option<IpAddr> {
    let socket = UdpSocket::bind(bind_addr).ok()?;
    socket.connect(probe_addr).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        None
    } else {
        Some(ip)
    }
}

fn url_host_component(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') && !host.ends_with(']') {
        return format!("[{host}]");
    }
    host.to_string()
}

fn serve_init_artifact_labels(
    token_path: &Path,
    cert_path: &Path,
    key_path: &Path,
) -> (Option<String>, String, String, String) {
    let common_parent = token_path.parent().and_then(|parent| {
        if cert_path.parent() == Some(parent) && key_path.parent() == Some(parent) {
            Some(parent)
        } else {
            None
        }
    });
    if let Some(parent) = common_parent {
        return (
            Some(display_pool_dir_for_humans(parent)),
            display_artifact_name(token_path),
            display_artifact_name(cert_path),
            display_artifact_name(key_path),
        );
    }
    (
        None,
        display_handoff_path_from_path(token_path),
        display_handoff_path_from_path(cert_path),
        display_handoff_path_from_path(key_path),
    )
}

fn display_artifact_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .unwrap_or_else(|| display_handoff_path_from_path(path))
}

fn display_handoff_path_from_path(path: &Path) -> String {
    let to_dot_relative = |value: &Path| {
        let rendered = value.display().to_string();
        if rendered.starts_with("./") || rendered.starts_with("../") {
            rendered
        } else {
            format!("./{rendered}")
        }
    };

    if path.is_relative() {
        return to_dot_relative(path);
    }
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(relative) = path.strip_prefix(&cwd)
        && !relative.as_os_str().is_empty()
    {
        return to_dot_relative(relative);
    }
    path.display().to_string()
}

fn display_pool_dir_for_humans(pool_dir: &Path) -> String {
    let rendered = if let Ok(cwd) = std::env::current_dir()
        && let Ok(relative) = pool_dir.strip_prefix(&cwd)
        && !relative.as_os_str().is_empty()
    {
        format!("./{}", relative.display())
    } else if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(relative) = pool_dir.strip_prefix(home)
        && !relative.as_os_str().is_empty()
    {
        format!("~/{}", relative.display())
    } else {
        pool_dir.display().to_string()
    };
    if rendered.ends_with('/') {
        rendered
    } else {
        format!("{rendered}/")
    }
}

fn extract_bind_from_server_commands(commands: &[String]) -> Option<String> {
    let command = commands.first()?;
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    tokens
        .windows(2)
        .find(|window| window[0] == "--bind")
        .map(|window| window[1].to_string())
}

fn emit_serve_startup_guidance(config: &serve::ServeConfig) {
    if !io::stderr().is_terminal() {
        return;
    }
    for line in build_serve_startup_lines(config) {
        eprintln!("{line}");
    }
}

fn build_serve_startup_lines(config: &serve::ServeConfig) -> Vec<String> {
    let tls_enabled = serve_tls_enabled(config);
    let scheme = serve_scheme(config);
    let host = display_host(config.bind.ip());
    let base_url = format!("{scheme}://{host}:{}", config.bind.port());
    let web_ui_url = format!("{base_url}/ui");
    let append_url = format!("{base_url}/v0/pools/demo/append");
    let curl_tls_flag = if config.tls_self_signed { " -k" } else { "" };
    let scope = serve_scope(config.bind.ip());
    let auth = if config.token.is_some() {
        "bearer"
    } else {
        "none"
    };
    let tls = if config.tls_self_signed {
        "self-signed"
    } else if tls_enabled {
        "on"
    } else {
        "off"
    };
    let access = match config.access_mode {
        serve::AccessMode::ReadOnly => "read-only",
        serve::AccessMode::WriteOnly => "write-only",
        serve::AccessMode::ReadWrite => "read-write",
    };
    let cors = if config.cors_allowed_origins.is_empty() {
        "same-origin"
    } else {
        "allowlist"
    };

    let mut feed_cmd = format!("pls feed {base_url}/demo");
    let mut follow_cmd = format!("pls follow {base_url}/demo");
    if config.token.is_some() {
        if config.token_file_used {
            feed_cmd.push_str(" --token-file <token-file>");
            follow_cmd.push_str(" --token-file <token-file>");
        } else {
            feed_cmd.push_str(" --token <token>");
            follow_cmd.push_str(" --token <token>");
        }
    }
    if tls_enabled {
        feed_cmd.push_str(" --tls-ca <tls-cert>");
        follow_cmd.push_str(" --tls-ca <tls-cert>");
    }
    feed_cmd.push_str(" '{\"hello\":\"world\"}'");
    follow_cmd.push_str(" --tail 10");

    let mut lines = vec![
        format!("Serving pools on {base_url} ({scope})"),
        String::new(),
        format!("  UI:   {web_ui_url}"),
        format!("  Auth: {auth}    TLS: {tls}    Access: {access}    CORS: {cors}"),
    ];

    if let Some(fingerprint) = config.tls_fingerprint.as_deref() {
        lines.push(format!("  Fingerprint: {fingerprint}"));
    }

    lines.push(String::new());
    lines.push("Try it:".to_string());
    lines.push(String::new());
    lines.push(format!("  {feed_cmd}"));
    lines.push(format!("  {follow_cmd}"));
    lines.push(String::new());
    if config.token.is_some() && config.token_file_used {
        lines.push("  TOKEN=$(cat <token-file>)".to_string());
    }
    let auth_header = if config.token.is_some() {
        if config.token_file_used {
            " -H \"Authorization: Bearer $TOKEN\""
        } else {
            " -H 'Authorization: Bearer <token>'"
        }
    } else {
        ""
    };
    lines.push(format!(
        "  curl{curl_tls_flag} -sS -X POST{auth_header} -H 'content-type: application/json' \\"
    ));
    lines.push("    --data '{\"hello\":\"world\"}' \\".to_string());
    lines.push(format!("    '{append_url}'"));
    lines.push(String::new());
    lines.push("Press Ctrl-C to stop.".to_string());

    if config.token.is_some() && config.token_file_used {
        lines.push(String::new());
        lines.push(
            "The token is in the file, not printed here. Share token and fingerprint out-of-band."
                .to_string(),
        );
    }

    if config.tls_self_signed {
        lines.push("Self-signed TLS: clients should trust the cert with --tls-ca.".to_string());
    }
    if config.bind.ip().is_unspecified() {
        lines.push(String::new());
        lines.push(
            "Replace YOUR-HOST/127.0.0.1 with your host IP or DNS name for remote clients."
                .to_string(),
        );
    }
    lines
}

fn emit_serve_check_report(config: &serve::ServeConfig, color_mode: ColorMode, json: bool) {
    if !json {
        for line in build_serve_check_lines(config) {
            println!("{line}");
        }
        return;
    }

    let tls_enabled = serve_tls_enabled(config);
    let base_url = format!(
        "{}://{}:{}",
        serve_scheme(config),
        display_host(config.bind.ip()),
        config.bind.port()
    );
    let auth_mode = if config.token.is_some() {
        if config.token_file_used {
            "bearer token (--token-file)"
        } else {
            "bearer token (--token)"
        }
    } else {
        "none"
    };
    let tls_mode = if config.tls_self_signed {
        "self-signed"
    } else if tls_enabled {
        "enabled"
    } else {
        "disabled"
    };
    let access_mode = match config.access_mode {
        serve::AccessMode::ReadOnly => "read-only",
        serve::AccessMode::WriteOnly => "write-only",
        serve::AccessMode::ReadWrite => "read-write",
    };
    let cors_origins = config.cors_allowed_origins.clone();

    emit_json(
        json!({
            "check": {
                "status": "valid",
                "listen": config.bind.to_string(),
                "base_url": base_url,
                "web_ui": format!("{base_url}/ui"),
                "web_ui_pool": format!("{base_url}/ui/pools/demo"),
                "auth": auth_mode,
                "tls": tls_mode,
                "tls_fingerprint": config.tls_fingerprint,
                "access": access_mode,
                "cors_allowed_origins": cors_origins,
                "limits": {
                    "max_body_bytes": config.max_body_bytes,
                    "max_tail_timeout_ms": config.max_tail_timeout_ms,
                    "max_tail_concurrency": config.max_concurrent_tails
                }
            }
        }),
        color_mode,
    );
}

fn build_serve_check_lines(config: &serve::ServeConfig) -> Vec<String> {
    let tls_enabled = serve_tls_enabled(config);
    let auth = if config.token.is_some() {
        "bearer token"
    } else {
        "none"
    };
    let tls = if config.tls_self_signed {
        "self-signed"
    } else if tls_enabled {
        "on"
    } else {
        "off"
    };
    let access = match config.access_mode {
        serve::AccessMode::ReadOnly => "access: read-only",
        serve::AccessMode::WriteOnly => "access: write-only",
        serve::AccessMode::ReadWrite => "access: read-write",
    };
    let access = access.strip_prefix("access: ").unwrap_or(access);
    let cors = if config.cors_allowed_origins.is_empty() {
        "same-origin"
    } else {
        "allowlist"
    };
    let mut lines = vec![
        "Configuration valid.".to_string(),
        String::new(),
        format!(
            "  Bind:   {} ({})",
            config.bind,
            serve_scope(config.bind.ip())
        ),
        format!("  Auth: {auth}    TLS: {tls}    Access: {access}    CORS: {cors}"),
        format!(
            "  Limits: body {}, timeout {}, concurrency {}",
            format_bytes(config.max_body_bytes),
            format_timeout_ms(config.max_tail_timeout_ms),
            config.max_concurrent_tails
        ),
    ];
    if let Some(fingerprint) = config.tls_fingerprint.as_deref() {
        lines.push(format!("  Fingerprint: {fingerprint}"));
    }
    lines.push(String::new());
    lines.push("Start with: pls serve".to_string());

    lines
}

fn serve_scope(ip: std::net::IpAddr) -> &'static str {
    if ip.is_loopback() {
        "loopback only"
    } else if ip.is_unspecified() {
        "all interfaces"
    } else {
        "network reachable"
    }
}

fn serve_scheme(config: &serve::ServeConfig) -> &'static str {
    if serve_tls_enabled(config) {
        "https"
    } else {
        "http"
    }
}

fn serve_tls_enabled(config: &serve::ServeConfig) -> bool {
    config.tls_self_signed || (config.tls_cert.is_some() && config.tls_key.is_some())
}

fn serve_config_from_run_args(
    run: ServeRunArgs,
    pool_dir: &Path,
) -> Result<serve::ServeConfig, Error> {
    let bind: SocketAddr = run.bind.parse().map_err(|_| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid bind address")
            .with_hint("Use a host:port value like 127.0.0.1:9700.")
    })?;
    if run.token.is_some() && run.token_file.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--token cannot be combined with --token-file")
            .with_hint("Use --token for dev, or run `plasmite serve init` and use the generated --token-file for safer deployments."));
    }
    let (token, token_file_used) = if let Some(path) = run.token_file {
        (Some(read_token_file(&path)?), true)
    } else {
        (run.token, false)
    };
    let tls_self_signed_material = if run.tls_self_signed {
        Some(serve::prepare_self_signed_tls(bind.ip())?)
    } else {
        None
    };
    let tls_fingerprint = if let Some(material) = &tls_self_signed_material {
        Some(material.fingerprint.clone())
    } else if let Some(cert_path) = run.tls_cert.as_ref() {
        Some(serve::tls_fingerprint_from_cert_path(cert_path)?)
    } else {
        None
    };
    Ok(serve::ServeConfig {
        bind,
        pool_dir: pool_dir.to_path_buf(),
        token,
        cors_allowed_origins: run.cors_origin,
        access_mode: run.access.into(),
        allow_non_loopback: run.allow_non_loopback,
        insecure_no_tls: run.insecure_no_tls,
        token_file_used,
        tls_cert: run.tls_cert,
        tls_key: run.tls_key,
        tls_self_signed: run.tls_self_signed,
        tls_self_signed_material,
        tls_fingerprint,
        max_body_bytes: run.max_body_bytes,
        max_tail_timeout_ms: run.max_tail_timeout_ms,
        max_concurrent_tails: run.max_tail_concurrency,
    })
}

fn format_timeout_ms(timeout_ms: u64) -> String {
    if timeout_ms.is_multiple_of(1000) {
        return format!("{}s", timeout_ms / 1000);
    }
    format!("{timeout_ms}ms")
}

fn display_host(ip: std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(addr) => {
            if addr.is_unspecified() {
                "127.0.0.1".to_string()
            } else {
                addr.to_string()
            }
        }
        std::net::IpAddr::V6(addr) => {
            let shown = if addr.is_unspecified() {
                "::1".to_string()
            } else {
                addr.to_string()
            };
            format!("[{shown}]")
        }
    }
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

fn emit_pool_info_pretty(pool_ref: &str, info: &plasmite::api::PoolInfo) {
    if !io::stdout().is_terminal() {
        println!("Pool: {pool_ref}");
        println!("Path: {}", info.path.display());
        println!(
            "Size: {} bytes (index: offset={} slots={} bytes={}, ring: offset={} size={})",
            info.file_size,
            info.index_offset,
            info.index_capacity,
            info.index_size_bytes,
            info.ring_offset,
            info.ring_size
        );

        let oldest = info
            .bounds
            .oldest_seq
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let newest = info
            .bounds
            .newest_seq
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let count = info
            .metrics
            .as_ref()
            .map(|metrics| metrics.message_count)
            .unwrap_or_else(|| match (info.bounds.oldest_seq, info.bounds.newest_seq) {
                (Some(oldest), Some(newest)) => newest.saturating_sub(oldest).saturating_add(1),
                _ => 0,
            });
        println!("Bounds: oldest={oldest} newest={newest} count={count}");

        if let Some(metrics) = &info.metrics {
            let whole = metrics.utilization.used_percent_hundredths / 100;
            let frac = metrics.utilization.used_percent_hundredths % 100;
            println!(
                "Utilization: used={}B free={}B ({}.{:02}%)",
                metrics.utilization.used_bytes, metrics.utilization.free_bytes, whole, frac
            );
            println!(
                "Oldest: {} ({})",
                metrics.age.oldest_time.as_deref().unwrap_or("-"),
                human_age(metrics.age.oldest_age_ms),
            );
            println!(
                "Newest: {} ({})",
                metrics.age.newest_time.as_deref().unwrap_or("-"),
                human_age(metrics.age.newest_age_ms),
            );
        }
        return;
    }

    let count = message_count_from_info(info);
    println!("{pool_ref}");
    println!(
        "  path:      {}",
        short_display_path(&info.path, info.path.parent())
    );
    let messages_summary =
        format_pool_messages_summary(count, info.bounds.oldest_seq, info.bounds.newest_seq);
    if let Some(metrics) = &info.metrics {
        let whole = metrics.utilization.used_percent_hundredths / 100;
        let frac = metrics.utilization.used_percent_hundredths % 100;
        println!(
            "  size:      {} ({} used, {}.{:02}%)",
            format_bytes(info.file_size),
            format_bytes(metrics.utilization.used_bytes),
            whole,
            frac
        );
        println!("  messages:  {messages_summary}");
        println!(
            "  oldest:    {}",
            format_pool_time_summary(
                metrics.age.oldest_age_ms,
                metrics.age.oldest_time.as_deref()
            )
        );
        println!(
            "  newest:    {}",
            format_pool_time_summary(
                metrics.age.newest_age_ms,
                metrics.age.newest_time.as_deref()
            )
        );
    } else {
        println!("  size:      {}", format_bytes(info.file_size));
        println!("  messages:  {messages_summary}");
    }
    println!(
        "  index:     {} slots ({})",
        info.index_capacity,
        format_bytes(info.index_size_bytes)
    );
    println!("  ring:      {}", format_bytes(info.ring_size));
}

fn message_count_from_info(info: &plasmite::api::PoolInfo) -> u64 {
    info.metrics
        .as_ref()
        .map(|metrics| metrics.message_count)
        .unwrap_or_else(|| {
            message_count_from_bounds(info.bounds.oldest_seq, info.bounds.newest_seq)
        })
}

fn message_count_from_bounds(oldest: Option<u64>, newest: Option<u64>) -> u64 {
    match (oldest, newest) {
        (Some(oldest), Some(newest)) if newest >= oldest => {
            newest.saturating_sub(oldest).saturating_add(1)
        }
        _ => 0,
    }
}

fn format_pool_messages_summary(count: u64, oldest: Option<u64>, newest: Option<u64>) -> String {
    let seq_range = format_seq_range(oldest, newest);
    if count == 0 {
        if seq_range == "-" {
            return "empty".to_string();
        }
        return format!("0 visible ({seq_range})");
    }
    if seq_range == "-" {
        return count.to_string();
    }
    format!("{count} ({seq_range})")
}

fn format_pool_time_summary(age_ms: Option<u64>, timestamp: Option<&str>) -> String {
    let Some(timestamp) = timestamp else {
        return "—".to_string();
    };
    format!(
        "{} ({})",
        format_relative_time(age_ms),
        format_timestamp_human(timestamp)
    )
}

fn emit_feed_receipt_human(receipt: &Value) {
    let seq = receipt
        .get("seq")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let time = receipt
        .get("time")
        .and_then(|value| value.as_str())
        .map(format_timestamp_human)
        .unwrap_or_else(|| "-".to_string());
    let tags = receipt
        .get("meta")
        .and_then(|value| value.get("tags"))
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    if tags.is_empty() {
        println!("fed seq={seq} at {time}");
    } else {
        println!("fed seq={seq} at {time}  tags: {tags}");
    }
}

fn emit_feed_receipt(value: Value, color_mode: ColorMode) {
    if io::stdout().is_terminal() {
        emit_feed_receipt_human(&value);
    } else {
        emit_json(value, color_mode);
    }
}

fn emit_version_output(color_mode: ColorMode) {
    if io::stdout().is_terminal() {
        println!("plasmite {}", env!("CARGO_PKG_VERSION"));
    } else {
        emit_json(
            json!({
                "name": "plasmite",
                "version": env!("CARGO_PKG_VERSION"),
            }),
            color_mode,
        );
    }
}

fn short_display_path(path: &Path, base_dir: Option<&Path>) -> String {
    if let Some(base) = base_dir {
        if let Ok(relative) = path.strip_prefix(base) {
            if !relative.as_os_str().is_empty() {
                return relative.display().to_string();
            }
        }
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

fn emit_table(headers: &[&str], rows: &[Vec<String>]) {
    println!("{}", render_table(headers, rows));
}

fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if headers.is_empty() {
        return String::new();
    }
    let column_count = headers.len();
    let mut sanitized_rows = Vec::with_capacity(rows.len());
    let mut widths = headers
        .iter()
        .map(|header| header.chars().count())
        .collect::<Vec<_>>();

    for row in rows {
        let mut sanitized = Vec::with_capacity(column_count);
        for (idx, width) in widths.iter_mut().enumerate() {
            let value = row.get(idx).map(String::as_str).unwrap_or("");
            let cleaned = sanitize_table_cell(value);
            *width = (*width).max(cleaned.chars().count());
            sanitized.push(cleaned);
        }
        sanitized_rows.push(sanitized);
    }

    let mut lines = Vec::with_capacity(sanitized_rows.len() + 1);
    lines.push(format_table_line(
        &headers
            .iter()
            .map(|header| header.to_string())
            .collect::<Vec<_>>(),
        &widths,
    ));
    for row in sanitized_rows {
        lines.push(format_table_line(&row, &widths));
    }
    lines.join("\n")
}

fn sanitize_table_cell(value: &str) -> String {
    value.replace('\n', "\\n").replace('\r', "\\r")
}

fn format_table_line(cells: &[String], widths: &[usize]) -> String {
    let mut line = String::new();
    for (idx, width) in widths.iter().enumerate() {
        if idx > 0 {
            line.push_str("  ");
        }
        let cell = cells.get(idx).map(String::as_str).unwrap_or("");
        line.push_str(cell);
        let cell_len = cell.chars().count();
        if *width > cell_len {
            line.push_str(&" ".repeat(*width - cell_len));
        }
    }
    line
}

fn human_age(age_ms: Option<u64>) -> String {
    format_relative_time(age_ms)
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
        let label = colorize_label("notice:", color_mode.use_color(is_tty), AnsiColor::Yellow);
        if notice.cmd == "feed" {
            eprintln!("{label} {}", notice.message);
        } else {
            eprintln!("{label} {} (pool: {})", notice.message, notice.pool);
        }
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
            display_handoff_path_from_path(path)
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

fn emit_follow_timeout_human(timeout_label: &str) {
    if io::stderr().is_terminal() {
        eprintln!("No messages received (timed out after {timeout_label}).");
    }
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
    let missing_required = rendered.contains("required arguments were not provided")
        || rendered.contains("required argument was not provided");
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

    let required_tokens: Vec<&str> = tokens
        .iter()
        .skip(pos + 1 + parts.len())
        .copied()
        .filter(|token| token.starts_with('<') && token.ends_with('>'))
        .collect();
    if missing_required
        && parts.as_slice() == ["follow"]
        && required_tokens
            .iter()
            .any(|token| token.contains("POOL") || token.contains("pool"))
    {
        return "Provide a pool ref, for example: `plasmite follow chat -n 1`.".to_string();
    }

    format!("Try `plasmite {} --help`.", parts.join(" "))
}

fn parse_inline_json(data: &str) -> Result<Value, Error> {
    serde_json::from_str(data).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid json")
            .with_hint("Provide a single JSON value (e.g. '{\"x\":1}').")
            .with_source(err)
    })
}

fn missing_feed_data_error() -> Error {
    Error::new(ErrorKind::Usage)
        .with_message("missing data input")
        .with_hint("Provide JSON via DATA, --file, or pipe JSON to stdin.")
}

fn open_feed_reader(path: &str) -> Result<Box<dyn Read>, Error> {
    if path == "-" {
        return Ok(Box::new(io::stdin()));
    }
    let reader = std::fs::File::open(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read data file")
            .with_path(path)
            .with_source(err)
    })?;
    Ok(Box::new(reader))
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
    pool_path_label: &str,
    color_mode: ColorMode,
) {
    let mut details = Map::new();
    details.insert("mode".to_string(), json!(mode_label(failure.mode)));
    details.insert("index".to_string(), json!(failure.index));
    details.insert("error_kind".to_string(), json!(failure.error_kind));
    details.insert("pool_path".to_string(), json!(pool_path_label));
    if let Some(line) = failure.line {
        details.insert("line".to_string(), json!(line));
    }
    if let Some(snippet) = &failure.snippet {
        details.insert("snippet".to_string(), json!(snippet));
    }
    let notice = Notice {
        kind: "ingest_skip".to_string(),
        time: notice_time_now().unwrap_or_else(|| "unknown".to_string()),
        cmd: "feed".to_string(),
        pool: pool_ref.to_string(),
        message: ingest_failure_message(failure),
        details,
    };
    emit_notice(&notice, color_mode);
}

fn ingest_failure_message(failure: &IngestFailure) -> String {
    match failure.error_kind.as_str() {
        "Parse" => "Skipped invalid JSON.".to_string(),
        "Oversize" => "Skipped oversized record.".to_string(),
        _ => format!("Skipped record: {}.", failure.message),
    }
}

fn ingest_summary_notice(
    outcome: &IngestOutcome,
    pool_ref: &str,
    pool_path_label: &str,
    color_mode: ColorMode,
) {
    let mut details = Map::new();
    details.insert("total".to_string(), json!(outcome.records_total));
    details.insert("ok".to_string(), json!(outcome.ok));
    details.insert("failed".to_string(), json!(outcome.failed));
    details.insert("pool_path".to_string(), json!(pool_path_label));
    let notice = Notice {
        kind: "ingest_summary".to_string(),
        time: notice_time_now().unwrap_or_else(|| "unknown".to_string()),
        cmd: "feed".to_string(),
        pool: pool_ref.to_string(),
        message: format!(
            "Finished with {} skipped record{}.",
            outcome.failed,
            if outcome.failed == 1 { "" } else { "s" }
        ),
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

struct FeedIngestContext<'a> {
    pool_ref: &'a str,
    pool_path_label: &'a str,
    tags: &'a [String],
    durability: Durability,
    retry_config: Option<RetryConfig>,
    pool_handle: &'a mut Pool,
    color_mode: ColorMode,
    input: InputMode,
    errors: ErrorPolicyCli,
}

struct RemoteFeedIngestContext<'a> {
    pool_ref: &'a str,
    pool_path_label: &'a str,
    tags: &'a [String],
    durability: Durability,
    retry_config: Option<RetryConfig>,
    remote_pool: &'a RemotePool,
    color_mode: ColorMode,
    input: InputMode,
    errors: ErrorPolicyCli,
}

fn ingest_from_stdin<R: Read>(
    reader: R,
    ctx: FeedIngestContext<'_>,
    emit_receipt: bool,
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
            let payload = lite3::encode_message(ctx.tags, &data)?;
            let (seq, timestamp_ns) = retry_with_config(ctx.retry_config, || {
                let timestamp_ns = now_ns()?;
                let options = AppendOptions::new(timestamp_ns, ctx.durability);
                let seq = ctx
                    .pool_handle
                    .append_with_options(payload.as_slice(), options)?;
                Ok((seq, timestamp_ns))
            })?;
            if emit_receipt {
                emit_feed_receipt(
                    feed_receipt_json(seq, timestamp_ns, ctx.tags)?,
                    ctx.color_mode,
                );
            }
            Ok(())
        },
        |failure| {
            ingest_failure_notice(&failure, ctx.pool_ref, ctx.pool_path_label, ctx.color_mode)
        },
    )?;

    if ctx.errors == ErrorPolicyCli::Skip && outcome.failed > 0 {
        ingest_summary_notice(&outcome, ctx.pool_ref, ctx.pool_path_label, ctx.color_mode);
    }

    Ok(outcome)
}

fn ingest_from_stdin_remote<R: Read>(
    reader: R,
    ctx: RemoteFeedIngestContext<'_>,
    emit_receipt: bool,
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
            let message = retry_with_config(ctx.retry_config, || {
                ctx.remote_pool
                    .append_json_now(&data, ctx.tags, ctx.durability)
            })?;
            if emit_receipt {
                emit_feed_receipt(feed_receipt_from_message(&message), ctx.color_mode);
            }
            Ok(())
        },
        |failure| {
            ingest_failure_notice(&failure, ctx.pool_ref, ctx.pool_path_label, ctx.color_mode)
        },
    )?;

    if ctx.errors == ErrorPolicyCli::Skip && outcome.failed > 0 {
        ingest_summary_notice(&outcome, ctx.pool_ref, ctx.pool_path_label, ctx.color_mode);
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

fn feed_receipt_json(seq: u64, timestamp_ns: u64, tags: &[String]) -> Result<Value, Error> {
    Ok(json!({
        "seq": seq,
        "time": format_ts(timestamp_ns)?,
        "meta": {
            "tags": tags,
        },
    }))
}

fn feed_receipt_from_message(message: &plasmite::api::Message) -> Value {
    json!({
        "seq": message.seq,
        "time": message.time,
        "meta": {
            "tags": message.meta.tags,
        },
    })
}

fn message_to_json(message: &plasmite::api::Message) -> Value {
    json!({
        "seq": message.seq,
        "time": message.time,
        "meta": {
            "tags": message.meta.tags,
        },
        "data": message.data,
    })
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
    let doc = Lite3DocRef::new(payload);
    let meta_type = doc
        .type_at_key(0, "meta")
        .map_err(|err| err.with_message("missing meta"))?;
    if meta_type != lite3::sys::LITE3_TYPE_OBJECT {
        return Err(Error::new(ErrorKind::Corrupt).with_message("meta is not object"));
    }

    let meta_ofs = doc
        .key_offset("meta")
        .map_err(|err| err.with_message("missing meta"))?;
    let tags_ofs = doc
        .key_offset_at(meta_ofs, "tags")
        .map_err(|err| err.with_message("missing meta.tags"))?;
    let tags_json = doc.to_json_at(tags_ofs, false)?;
    let tags_value: Value = serde_json::from_str(&tags_json).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid payload json")
            .with_source(err)
    })?;
    let tags = tags_value
        .as_array()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("meta.tags must be array"))?
        .iter()
        .map(|item| item.as_str().map(|s| s.to_string()))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array")
        })?;
    let meta = json!({ "tags": tags });

    let data_ofs = doc
        .key_offset("data")
        .map_err(|err| err.with_message("missing data"))?;
    let data_json = doc.to_json_at(data_ofs, false)?;
    let data: Value = serde_json::from_str(&data_json).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid payload json")
            .with_source(err)
    })?;
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
struct FollowConfig {
    tail: u64,
    pretty: bool,
    one: bool,
    timeout: Option<Duration>,
    data_only: bool,
    since_ns: Option<u64>,
    required_tags: Vec<String>,
    where_predicates: Vec<JqFilter>,
    quiet_drops: bool,
    notify: bool,
    color_mode: ColorMode,
    replay_speed: Option<f64>,
    suppress_sender: Option<String>,
    stop: Option<Arc<AtomicBool>>,
}

fn matches_required_tags(required_tags: &[String], message: &Value) -> bool {
    if required_tags.is_empty() {
        return true;
    }
    let Some(tags) = message
        .get("meta")
        .and_then(|meta| meta.get("tags"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    required_tags.iter().all(|required| {
        tags.iter()
            .any(|tag| tag.as_str().is_some_and(|value| value == required))
    })
}

fn should_suppress_sender(message: &Value, sender: &str) -> bool {
    message
        .get("data")
        .and_then(|data| data.get("from"))
        .and_then(Value::as_str)
        .is_some_and(|value| value == sender)
}

fn duplex_requires_me_when_tty(stdin_is_terminal: bool, me: Option<&str>) -> bool {
    stdin_is_terminal && me.is_none()
}

fn parse_duplex_tty_line(me: &str, line: &str) -> Option<Value> {
    let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
    if trimmed.trim().is_empty() {
        return None;
    }
    Some(json!({
        "from": me,
        "msg": trimmed,
    }))
}

fn should_suppress_message(cfg: &FollowConfig, message: &Value) -> bool {
    cfg.suppress_sender
        .as_deref()
        .is_some_and(|sender| should_suppress_sender(message, sender))
}

fn follow_should_stop(stop: Option<&Arc<AtomicBool>>) -> bool {
    stop.is_some_and(|flag| flag.load(Ordering::Acquire))
}

fn follow_remote(
    client: &RemoteClient,
    pool: &str,
    cfg: &FollowConfig,
) -> Result<RunOutcome, Error> {
    if cfg.replay_speed.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --replay")
            .with_hint("Use local follow with --replay, or omit --replay for remote streams."));
    }
    if cfg.since_ns.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --since")
            .with_hint("Use --tail N for remote refs, or run --since against a local pool path."));
    }
    if !cfg.notify {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --no-notify")
            .with_hint("--no-notify only applies to local pool semaphores."));
    }
    if cfg.quiet_drops {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --quiet-drops")
            .with_hint("--quiet-drops only applies to local drop notices."));
    }

    let remote_pool = client.open_pool(&PoolRef::name(pool))?;

    let mut next_since_seq = if cfg.tail > 0 {
        let info = remote_pool.info()?;
        match (info.bounds.oldest_seq, info.bounds.newest_seq) {
            (Some(oldest), Some(newest)) => Some(
                newest
                    .saturating_sub(cfg.tail.saturating_sub(1))
                    .max(oldest),
            ),
            _ => None,
        }
    } else {
        None
    };

    let mut tail_wait_matches = VecDeque::new();
    loop {
        if follow_should_stop(cfg.stop.as_ref()) {
            return Ok(RunOutcome::ok());
        }

        let mut options = TailOptions::new();
        options.since_seq = next_since_seq;
        options.timeout = cfg.timeout;
        let mut tail = remote_pool.tail(options)?;

        let mut emitted_in_cycle = false;
        while let Some(message) = tail.next_message()? {
            if follow_should_stop(cfg.stop.as_ref()) {
                return Ok(RunOutcome::ok());
            }
            next_since_seq = Some(message.seq.saturating_add(1));
            let value = message_to_json(&message);
            if should_suppress_message(cfg, &value)
                || !matches_required_tags(cfg.required_tags.as_slice(), &value)
                || !matches_all(cfg.where_predicates.as_slice(), &value)?
            {
                continue;
            }

            if cfg.one && cfg.tail > 0 {
                tail_wait_matches.push_back(value);
                while tail_wait_matches.len() > cfg.tail as usize {
                    tail_wait_matches.pop_front();
                }
                if tail_wait_matches.len() == cfg.tail as usize {
                    if let Some(latest) = tail_wait_matches.back() {
                        emit_message(
                            output_value(latest.clone(), cfg.data_only),
                            cfg.pretty,
                            cfg.color_mode,
                        );
                    }
                    return Ok(RunOutcome::ok());
                }
                emitted_in_cycle = true;
                continue;
            }

            emit_message(
                output_value(value, cfg.data_only),
                cfg.pretty,
                cfg.color_mode,
            );
            emitted_in_cycle = true;
            if cfg.one {
                return Ok(RunOutcome::ok());
            }
        }

        if cfg.timeout.is_some() && !emitted_in_cycle {
            return Ok(RunOutcome::with_code(124));
        }
    }
}

fn follow_pool(
    pool: &Pool,
    pool_ref: &str,
    pool_path: &Path,
    cfg: FollowConfig,
) -> Result<RunOutcome, Error> {
    if cfg.replay_speed.is_some() {
        return follow_replay(pool, &cfg);
    }

    let mut cursor = Cursor::new();
    let mut header = pool.header_from_mmap()?;
    let mut emit = VecDeque::new();
    let mut last_seen_seq = None::<u64>;
    let mut pending_drop: Option<DropNotice> = None;
    let mut last_notice_at: Option<Instant> = None;
    let notice_interval = Duration::from_secs(1);
    let tail_wait = cfg.one && cfg.tail > 0;
    let mut timeout_deadline = cfg.timeout.map(|duration| Instant::now() + duration);
    let mut notify_enabled = cfg.notify;

    let bump_timeout = |deadline: &mut Option<Instant>| {
        if let Some(duration) = cfg.timeout {
            *deadline = Some(Instant::now() + duration);
        }
    };

    if let Some(since_ns) = cfg.since_ns {
        cursor.seek_to(header.tail_off as usize);
        loop {
            if follow_should_stop(cfg.stop.as_ref()) {
                return Ok(RunOutcome::ok());
            }
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    if follow_should_stop(cfg.stop.as_ref()) {
                        return Ok(RunOutcome::ok());
                    }
                    if frame.timestamp_ns >= since_ns {
                        let message = message_from_frame(&frame)?;
                        if !should_suppress_message(&cfg, &message)
                            && matches_required_tags(cfg.required_tags.as_slice(), &message)
                            && matches_all(cfg.where_predicates.as_slice(), &message)?
                        {
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
            if follow_should_stop(cfg.stop.as_ref()) {
                return Ok(RunOutcome::ok());
            }
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    if follow_should_stop(cfg.stop.as_ref()) {
                        return Ok(RunOutcome::ok());
                    }
                    let message = message_from_frame(&frame)?;
                    if !should_suppress_message(&cfg, &message)
                        && matches_required_tags(cfg.required_tags.as_slice(), &message)
                        && matches_all(cfg.where_predicates.as_slice(), &message)?
                    {
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
    let pool_path_label = pool_path.display().to_string();

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
        details.insert("pool_path".to_string(), json!(pool_path_label.as_str()));
        let notice = Notice {
            kind: "drop".to_string(),
            time,
            cmd: "follow".to_string(),
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
        if follow_should_stop(cfg.stop.as_ref()) {
            return Ok(RunOutcome::ok());
        }
        match cursor.next(pool)? {
            CursorResult::Message(frame) => {
                if follow_should_stop(cfg.stop.as_ref()) {
                    return Ok(RunOutcome::ok());
                }
                if let Some(last_seen_seq) = last_seen_seq {
                    if frame.seq > last_seen_seq + 1 {
                        queue_drop(last_seen_seq, frame.seq, &mut pending_drop);
                        maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                    }
                }
                let message = message_from_frame(&frame)?;
                if !should_suppress_message(&cfg, &message)
                    && matches_required_tags(cfg.required_tags.as_slice(), &message)
                    && matches_all(cfg.where_predicates.as_slice(), &message)?
                {
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
                if follow_should_stop(cfg.stop.as_ref()) {
                    return Ok(RunOutcome::ok());
                }
                maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                if let Some(deadline) = timeout_deadline {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(RunOutcome::with_code(124));
                    }
                    let remaining = deadline.duration_since(now);
                    let wait_for = std::cmp::min(backoff, remaining);
                    if notify_enabled {
                        match notify::wait_for_path(pool_path, wait_for) {
                            NotifyWait::Signaled | NotifyWait::TimedOut => {}
                            NotifyWait::Unavailable => {
                                notify_enabled = false;
                                std::thread::sleep(wait_for);
                            }
                        }
                    } else {
                        std::thread::sleep(wait_for);
                    }
                } else if notify_enabled {
                    match notify::wait_for_path(pool_path, backoff) {
                        NotifyWait::Signaled | NotifyWait::TimedOut => {}
                        NotifyWait::Unavailable => {
                            notify_enabled = false;
                            std::thread::sleep(backoff);
                        }
                    }
                } else {
                    std::thread::sleep(backoff);
                }
                backoff = std::cmp::min(backoff * 2, max_backoff);
            }
            CursorResult::FellBehind => {
                if follow_should_stop(cfg.stop.as_ref()) {
                    return Ok(RunOutcome::ok());
                }
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

fn follow_replay(pool: &Pool, cfg: &FollowConfig) -> Result<RunOutcome, Error> {
    let speed = cfg.replay_speed.unwrap_or(0.0);
    let mut cursor = Cursor::new();
    let mut header = pool.header_from_mmap()?;
    let mut collected: Vec<(u64, Value)> = Vec::new();

    if let Some(since_ns) = cfg.since_ns {
        cursor.seek_to(header.tail_off as usize);
        loop {
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    if frame.timestamp_ns >= since_ns {
                        let message = message_from_frame(&frame)?;
                        if matches_required_tags(cfg.required_tags.as_slice(), &message)
                            && matches_all(cfg.where_predicates.as_slice(), &message)?
                        {
                            collected.push((frame.timestamp_ns, message));
                        }
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
    } else {
        cursor.seek_to(header.tail_off as usize);
        let mut buffer: VecDeque<(u64, Value)> = VecDeque::new();
        loop {
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    let message = message_from_frame(&frame)?;
                    if matches_required_tags(cfg.required_tags.as_slice(), &message)
                        && matches_all(cfg.where_predicates.as_slice(), &message)?
                    {
                        if cfg.tail > 0 {
                            buffer.push_back((frame.timestamp_ns, message));
                            while buffer.len() > cfg.tail as usize {
                                buffer.pop_front();
                            }
                        } else {
                            collected.push((frame.timestamp_ns, message));
                        }
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
        if cfg.tail > 0 {
            collected = buffer.into_iter().collect();
        }
    }

    if collected.is_empty() {
        return Ok(RunOutcome::ok());
    }

    let mut prev_ts = collected[0].0;
    for (i, (ts, message)) in collected.into_iter().enumerate() {
        if i > 0 && speed > 0.0 {
            let delta_ns = ts.saturating_sub(prev_ts);
            let delay_ns = (delta_ns as f64 / speed) as u64;
            if delay_ns > 0 {
                std::thread::sleep(Duration::from_nanos(delay_ns));
            }
        }
        emit_message(
            output_value(message, cfg.data_only),
            cfg.pretty,
            cfg.color_mode,
        );
        prev_ts = ts;
        if cfg.one {
            return Ok(RunOutcome::ok());
        }
    }

    Ok(RunOutcome::ok())
}

#[cfg(test)]
mod tests {
    use super::{
        Error, ErrorKind, PoolTarget, RetryConfig, build_serve_startup_lines,
        duplex_requires_me_when_tty, error_text, format_bytes, format_relative_time,
        format_seq_range, format_timestamp_human, matches_required_tags, parse_duplex_tty_line,
        parse_duration, parse_size, read_token_file, render_table, resolve_pool_target,
        retry_with_config, short_display_path,
    };
    use serde_json::json;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tempfile::NamedTempFile;

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

    fn test_serve_config() -> super::serve::ServeConfig {
        super::serve::ServeConfig {
            bind: "127.0.0.1:9700".parse().expect("bind"),
            pool_dir: PathBuf::from("/tmp/pools"),
            token: None,
            cors_allowed_origins: Vec::new(),
            access_mode: super::serve::AccessMode::ReadWrite,
            allow_non_loopback: false,
            insecure_no_tls: false,
            token_file_used: false,
            tls_cert: None,
            tls_key: None,
            tls_self_signed: false,
            tls_self_signed_material: None,
            tls_fingerprint: None,
            max_body_bytes: 1024 * 1024,
            max_tail_timeout_ms: 30_000,
            max_concurrent_tails: 64,
        }
    }

    #[test]
    fn serve_startup_banner_secure_mode_includes_clients_section() {
        let mut config = test_serve_config();
        config.token = Some("secret".to_string());
        config.token_file_used = true;
        config.tls_self_signed = true;
        config.tls_fingerprint = Some("SHA256:AA:BB".to_string());
        let text = build_serve_startup_lines(&config).join("\n");
        assert!(text.contains("Serving pools on https://127.0.0.1:9700 (loopback only)"));
        assert!(text.contains("Auth: bearer    TLS: self-signed"));
        assert!(text.contains("--token-file <token-file> --tls-ca <tls-cert>"));
        assert!(text.contains("Fingerprint: SHA256:AA:BB"));
    }

    #[test]
    fn serve_startup_banner_local_mode_stays_compact() {
        let config = test_serve_config();
        let text = build_serve_startup_lines(&config).join("\n");
        assert!(text.contains("Serving pools on http://127.0.0.1:9700 (loopback only)"));
        assert!(text.contains("Auth: none    TLS: off    Access: read-write    CORS: same-origin"));
        assert!(text.contains("Try it:"));
        assert!(text.contains("Press Ctrl-C to stop."));
    }

    #[test]
    fn format_bytes_boundaries() {
        assert_eq!(format_bytes(0), "0");
        assert_eq!(format_bytes(1023), "1023");
        assert_eq!(format_bytes(1024), "1K");
        assert_eq!(format_bytes(1536), "1.5K");
        assert_eq!(format_bytes(1024 * 1024), "1M");
    }

    #[test]
    fn format_timestamp_human_truncates_to_seconds() {
        assert_eq!(
            format_timestamp_human("2026-02-27T12:34:56.789Z"),
            "2026-02-27T12:34:56Z"
        );
        assert_eq!(format_timestamp_human(""), "-");
    }

    #[test]
    fn format_relative_time_boundaries() {
        assert_eq!(format_relative_time(Some(0)), "1s ago");
        assert_eq!(format_relative_time(Some(59_000)), "59s ago");
        assert_eq!(format_relative_time(Some(60_000)), "1m ago");
        assert_eq!(format_relative_time(Some(3_600_000)), "1h ago");
        assert_eq!(format_relative_time(Some(86_400_000)), "1d ago");
        assert_eq!(format_relative_time(Some(7 * 86_400_000)), "1w ago");
        assert_eq!(format_relative_time(None), "-");
    }

    #[test]
    fn format_seq_range_handles_empty_and_present_bounds() {
        assert_eq!(format_seq_range(None, None), "-");
        assert_eq!(format_seq_range(Some(3), Some(5)), "seq 3..5");
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
    fn duplex_requires_me_when_tty_inputs() {
        assert!(duplex_requires_me_when_tty(true, None));
        assert!(!duplex_requires_me_when_tty(false, None));
        assert!(!duplex_requires_me_when_tty(true, Some("alice")));
        assert!(!duplex_requires_me_when_tty(false, Some("alice")));
    }

    #[test]
    fn parse_duplex_tty_line_supports_text_and_crlf() {
        let value = parse_duplex_tty_line("alice", "hello world\r\n").expect("value");
        assert_eq!(value.get("from").and_then(|v| v.as_str()), Some("alice"));
        assert_eq!(
            value.get("msg").and_then(|v| v.as_str()),
            Some("hello world")
        );

        assert!(parse_duplex_tty_line("alice", "\n").is_none());
        assert!(parse_duplex_tty_line("alice", "   \r\n").is_none());
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
    fn required_tags_match_all_requested_tags() {
        let message = json!({
            "meta": {
                "tags": ["error", "billing", "prod"]
            }
        });
        assert!(matches_required_tags(
            &["error".to_string(), "billing".to_string()],
            &message
        ));
        assert!(!matches_required_tags(
            &["error".to_string(), "missing".to_string()],
            &message
        ));
    }

    #[test]
    fn required_tags_returns_false_on_missing_meta_tags() {
        let message = json!({"data": {"x": 1}});
        assert!(!matches_required_tags(&["error".to_string()], &message));
    }

    #[test]
    fn token_file_trims_and_reads() {
        let mut file = NamedTempFile::new().expect("tempfile");
        std::io::Write::write_all(&mut file, b"  secret-token \n").expect("write");
        let token = read_token_file(file.path()).expect("token");
        assert_eq!(token, "secret-token");
    }

    #[test]
    fn token_file_rejects_empty() {
        let mut file = NamedTempFile::new().expect("tempfile");
        std::io::Write::write_all(&mut file, b" \n").expect("write");
        let err = read_token_file(file.path()).expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
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

    #[test]
    fn resolve_pool_target_classifies_local_name() {
        let pool_dir = Path::new("/tmp/pools");
        let target = resolve_pool_target("demo", pool_dir).expect("target");
        match target {
            PoolTarget::LocalPath(path) => assert_eq!(path, pool_dir.join("demo.plasmite")),
            _ => panic!("expected local path"),
        }
    }

    #[test]
    fn resolve_pool_target_accepts_remote_shorthand() {
        let target = resolve_pool_target("http://localhost:9170/demo", Path::new("/tmp/pools"))
            .expect("target");
        assert_eq!(
            target,
            PoolTarget::Remote {
                base_url: "http://localhost:9170/".to_string(),
                pool: "demo".to_string(),
            }
        );
    }

    #[test]
    fn resolve_pool_target_rejects_api_shaped_remote_ref() {
        let err = resolve_pool_target(
            "http://localhost:9170/v0/pools/demo/append",
            Path::new("/tmp/pools"),
        )
        .expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn resolve_pool_target_rejects_trailing_slash_remote_ref() {
        let err = resolve_pool_target("http://localhost:9170/demo/", Path::new("/tmp/pools"))
            .expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn resolve_pool_target_rejects_unsupported_scheme() {
        let err = resolve_pool_target("tcp://localhost:9170/demo", Path::new("/tmp/pools"))
            .expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn resolve_pool_target_rejects_query_and_fragment() {
        let err = resolve_pool_target("http://localhost:9170/demo?x=1", Path::new("/tmp/pools"))
            .expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
        let err = resolve_pool_target("http://localhost:9170/demo#frag", Path::new("/tmp/pools"))
            .expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[cfg(windows)]
    #[test]
    fn resolve_pool_target_treats_windows_backslash_path_as_local_path() {
        let input = r"C:\pools\demo.plasmite";
        let target = resolve_pool_target(input, Path::new("C:\\ignored")).expect("target");
        match target {
            PoolTarget::LocalPath(path) => assert_eq!(path, PathBuf::from(input)),
            _ => panic!("expected local path"),
        }
    }

    #[test]
    fn retry_with_config_retries_until_success() {
        let mut attempts = 0u32;
        let value = retry_with_config(
            Some(RetryConfig {
                retries: 2,
                delay: Duration::from_millis(0),
            }),
            || {
                attempts += 1;
                if attempts < 2 {
                    return Err(Error::new(ErrorKind::Busy));
                }
                Ok(21u8)
            },
        )
        .expect("retry should succeed");
        assert_eq!(attempts, 2);
        assert_eq!(value, 21u8);
    }

    #[test]
    fn retry_with_config_exhausts_when_still_retryable() {
        let mut attempts = 0u32;
        let result: Result<u8, Error> = retry_with_config(
            Some(RetryConfig {
                retries: 1,
                delay: Duration::from_millis(0),
            }),
            || {
                attempts += 1;
                Err(Error::new(ErrorKind::Busy))
            },
        );
        assert_eq!(attempts, 2);
        let err = result.expect_err("expected retry exhaustion");
        assert_eq!(err.kind(), ErrorKind::Busy);
        let hint = err.hint().unwrap_or("");
        assert!(hint.contains("Retry attempts: 2"));
    }

    #[test]
    fn render_table_aligns_and_sanitizes_cells() {
        let output = render_table(
            &["NAME", "DETAIL"],
            &[
                vec!["a".to_string(), "line1\nline2".to_string()],
                vec!["long-name".to_string(), "ok".to_string()],
            ],
        );
        let lines = output.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("NAME"));
        assert!(lines[0].contains("  DETAIL"));
        assert!(lines[1].contains("line1\\nline2"));
        assert!(lines[2].contains("long-name"));
        assert!(!lines[1].ends_with(' '));
    }

    #[test]
    fn short_display_path_prefers_relative_to_base_dir() {
        let path = PathBuf::from("/tmp/pools/demo.plasmite");
        let base = Path::new("/tmp/pools");
        assert_eq!(
            short_display_path(path.as_path(), Some(base)),
            "demo.plasmite".to_string()
        );
    }

    #[test]
    fn short_display_path_falls_back_to_basename() {
        let path = PathBuf::from("/tmp/pools/demo.plasmite");
        let other_base = Path::new("/different");
        assert_eq!(
            short_display_path(path.as_path(), Some(other_base)),
            "demo.plasmite".to_string()
        );
        assert_eq!(
            short_display_path(path.as_path(), None),
            "demo.plasmite".to_string()
        );
    }
}
