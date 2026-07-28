//! Purpose: Define the complete clap argument and help contract.
//! Exports: `Cli`, command enums, and command argument structures.
//! Role: Parse syntax only; command execution belongs to sibling CLI modules.

use crate::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_MAX_TAIL_CONCURRENCY, DEFAULT_MAX_TAIL_TIMEOUT_MS, serve,
};
use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use clap_complete::aot::Shell;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "plasmite",
    version,
    about = "Persistent JSON message pools for local and host-adjacent IPC",
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
    before_help = r#"A pool is a persistent, bounded stream that multiple processes can write and read.
Messages are JSON: `feed` appends, `follow` streams, and `fetch` reads one by sequence.
"#,
    after_help = r#"FIRST LOCAL WORKFLOW
  $ plasmite pool create chat
  $ plasmite follow chat                                      # Terminal 1
  $ plasmite feed chat '{"from":"alice","msg":"hello"}'       # Terminal 2

OUTPUT
  Commands default to readable terminal output. Use --json or --format jsonl
  where offered for scripts; command help describes adaptive output.

OPTIONS AND HELP
  Top-level options precede the command: plasmite --dir ./pools follow chat
  Command options follow it:              plasmite follow --tail 10 chat
  $ plasmite <command> --help

GUIDES
  CLI model: https://github.com/sandover/plasmite/blob/main/docs/cli.md
  Recipes:   https://github.com/sandover/plasmite/blob/main/docs/cookbook.md"#,
    arg_required_else_help = true,
    disable_help_subcommand = false
)]
pub(crate) struct Cli {
    #[arg(
        long,
        help = "Pool directory for named pools (default: ~/.plasmite/pools)",
        value_hint = ValueHint::DirPath
    )]
    pub(crate) dir: Option<PathBuf>,
    #[arg(
        long,
        default_value = "auto",
        value_enum,
        help = "Colorize stderr diagnostics and pretty JSON output: auto|always|never"
    )]
    pub(crate) color: ColorMode,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub(crate) enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub(crate) enum FollowFormat {
    Pretty,
    Jsonl,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum InputMode {
    Auto,
    Jsonl,
    Json,
    Seq,
    Jq,
}

#[derive(Copy, Clone, Debug, PartialEq, ValueEnum)]
pub(crate) enum ErrorPolicyCli {
    Stop,
    Skip,
}

impl ColorMode {
    pub(crate) fn use_color(self, is_tty: bool) -> bool {
        match self {
            ColorMode::Auto => is_tty,
            ColorMode::Always => true,
            ColorMode::Never => false,
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum Command {
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
  $ jq -c '.[]' data.json | plasmite feed foo                   # stream from pipe

INPUT AND OUTPUT
  - Choose one input source: inline DATA, --file, or stdin
  - Receipts are human-readable on a terminal and JSON when piped
  - --errors skip continues after bad records and exits 1 if any were rejected"#,
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
            help = "Pool size when creating (bytes or K/M/G; requires --create)"
        )]
        create_size: Option<String>,
        #[arg(long, default_value_t = 0, help = "Retry count for transient failures")]
        retry: u32,
        #[arg(
            long,
            help = "Delay between retries (e.g. 50ms, 1s, 2m; requires --retry > 0)"
        )]
        retry_delay: Option<String>,
        #[arg(
            short = 'i',
            long = "in",
            default_value = "auto",
            value_enum,
            help = "Input mode for file or stdin streams",
            long_help = r#"Input mode for file or stdin streams

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
            help = "File/stdin error policy: stop|skip"
        )]
        errors: ErrorPolicyCli,
        #[arg(
            long,
            help = "Bearer token for remote refs only (dev-only; prefer --token-file)",
            help_heading = "Remote auth/TLS"
        )]
        token: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Read bearer token from file for remote refs only",
            value_hint = ValueHint::FilePath,
            help_heading = "Remote auth/TLS"
        )]
        token_file: Option<PathBuf>,
        #[arg(
            long = "tls-ca",
            value_name = "PATH",
            help = "Trust this PEM CA/certificate for remote refs only",
            value_hint = ValueHint::FilePath,
            help_heading = "Remote auth/TLS"
        )]
        tls_ca: Option<PathBuf>,
        #[arg(
            long = "tls-skip-verify",
            help = "Disable TLS verification for remote refs only (unsafe; dev-only)",
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
  $ plasmite serve check                                        # validate config

CONSTRAINTS
  - Request body, tail timeout, and tail concurrency limits must be positive
  - `init` has its own artifact options; put serve options before `check`"#,
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
        about = "Serve MCP tools and resources on stdio",
        long_about = r#"Start an experimental MCP server on stdio.

Reads newline-delimited JSON-RPC requests from stdin and writes newline-delimited JSON-RPC responses to stdout.
The process exits when stdin closes."#,
        after_help = r#"EXAMPLES
  $ plasmite mcp
  $ plasmite mcp --dir /path/to/pools"#
    )]
    Mcp {
        #[arg(
            long,
            help = "Pool directory for named pools (default: ~/.plasmite/pools)",
            value_hint = ValueHint::DirPath
        )]
        dir: Option<PathBuf>,
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
  $ plasmite follow foo --format jsonl | jq '.data'               # pipe to jq

LOCAL AND REMOTE
  Remote refs support --tail, filters, --one, --timeout, output, and auth/TLS.
  They reject --create, --since, --replay, --no-notify, and --quiet-drops."#,
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
            help = "Replay local history at finite SPEED >= 0; requires --tail or --since"
        )]
        replay: Option<f64>,
        #[arg(
            long,
            help = "Bearer token for remote refs only (dev-only; prefer --token-file)",
            help_heading = "Remote auth/TLS"
        )]
        token: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Read bearer token from file for remote refs only",
            value_hint = ValueHint::FilePath,
            help_heading = "Remote auth/TLS"
        )]
        token_file: Option<PathBuf>,
        #[arg(
            long = "tls-ca",
            value_name = "PATH",
            help = "Trust this PEM CA/certificate for remote refs only",
            value_hint = ValueHint::FilePath,
            help_heading = "Remote auth/TLS"
        )]
        tls_ca: Option<PathBuf>,
        #[arg(
            long = "tls-skip-verify",
            help = "Disable TLS verification for remote refs only (unsafe; dev-only)",
            help_heading = "Remote auth/TLS"
        )]
        tls_skip_verify: bool,
    },
    #[command(
        arg_required_else_help = true,
        about = "Capture command output into a local pool",
        override_usage = "plasmite tap [OPTIONS] <POOL> -- <COMMAND>...",
        long_about = r#"Run a command, capture stdout/stderr as line messages, and append them to a local pool.

Use `--` to separate tap flags from the wrapped command argv."#,
        after_help = r#"EXAMPLES
  $ plasmite tap build --create -- cargo build
  $ plasmite follow build
  $ plasmite follow build --where '.data.stream == "stderr"'
  $ plasmite tap deploy --tag prod -- ./deploy.sh
  $ plasmite tap api --create --create-size 64M -- ./server

CAPTURE AND EXIT
  - `--` is required before wrapped command args
  - Use --create-size for long-running/high-volume captures
  - Emits start, stdout/stderr line, and exit messages
  - Returns the wrapped command's exit status; `tap` accepts local pools only"#
    )]
    Tap {
        #[arg(help = "Pool ref: local name/path")]
        pool: String,
        #[arg(long, help = "Create local pool if missing before tapping")]
        create: bool,
        #[arg(
            long = "create-size",
            help = "Pool size when creating (bytes or K/M/G; requires --create)"
        )]
        create_size: Option<String>,
        #[arg(long, help = "Repeatable tag for captured line messages")]
        tag: Vec<String>,
        #[arg(short = 'q', long, help = "Suppress child stdout/stderr passthrough")]
        quiet: bool,
        #[arg(long, default_value = "fast", help = "Durability mode: fast|flush")]
        durability: String,
        #[arg(
            last = true,
            allow_hyphen_values = true,
            value_name = "COMMAND",
            help = "Wrapped command and args (must follow `--`)"
        )]
        command: Vec<String>,
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
- Remote refs do not support `--create` or `--since` (use `--tail` for remote)."#,
        after_help = r#"INPUT AND EXIT
  - Terminal input requires --me and sends one chat message per non-empty line
  - Piped input is a JSON stream; remote refs expose no auth/TLS options
  - Exits 124 on timeout"#
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
        override_usage = "plasmite doctor [OPTIONS] <POOL|--all>",
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
        about = "Print version information",
        long_about = r#"Print human-readable version information on a terminal.

When stdout is redirected or piped, emit stable machine-readable JSON."#,
        after_help = r#"EXAMPLES
  $ plasmite version
  $ plasmite version | jq -r '.version'"#
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
pub(crate) enum AccessModeCli {
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
pub(crate) enum PoolCommand {
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
            help = "Inline index slots (default: auto; 0 disables; may use at most half the pool)"
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
pub(crate) enum ServeSubcommand {
    #[command(
        about = "Bootstrap secure serve token/TLS artifacts",
        long_about = r#"Generate token + TLS artifacts and print copy/paste next commands for secure serve startup."#,
        after_help = r#"EXAMPLES
  $ plasmite serve init
  $ plasmite serve init --output-dir ./.plasmite-serve
  $ plasmite serve init --output-dir ./.plasmite-serve --force

NOTES
  - Writes token/cert/key files without printing secret token values
  - Token, certificate, and key output paths must be distinct
  - Refuses to overwrite existing artifacts unless --force is set
  - Output is human-readable on a terminal and JSON when piped"#
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
  - Serve configuration options belong before `check`; only --json follows it
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
pub(crate) struct ServeInitArgs {
    #[arg(
        long,
        default_value = "0.0.0.0:9700",
        help = "Bind address used in printed next commands"
    )]
    pub(crate) bind: String,
    #[arg(
        long,
        default_value = ".",
        value_name = "PATH",
        help = "Base output directory for generated artifacts",
        value_hint = ValueHint::DirPath
    )]
    pub(crate) output_dir: PathBuf,
    #[arg(
        long,
        default_value = "plasmite-auth-token.txt",
        value_name = "PATH",
        help = "Token output path (relative to --output-dir unless absolute)",
        value_hint = ValueHint::FilePath
    )]
    pub(crate) token_file: PathBuf,
    #[arg(
        long = "tls-cert",
        default_value = "plasmite-tls-cert.pem",
        value_name = "PATH",
        help = "TLS certificate output path (relative to --output-dir unless absolute)",
        value_hint = ValueHint::FilePath
    )]
    pub(crate) tls_cert: PathBuf,
    #[arg(
        long = "tls-key",
        default_value = "plasmite-tls-key.pem",
        value_name = "PATH",
        help = "TLS private key output path (relative to --output-dir unless absolute)",
        value_hint = ValueHint::FilePath
    )]
    pub(crate) tls_key: PathBuf,
    #[arg(long, help = "Overwrite existing generated artifacts")]
    pub(crate) force: bool,
}

#[derive(Args)]
pub(crate) struct ServeRunArgs {
    #[arg(
        long,
        default_value = "127.0.0.1:9700",
        help = "Bind address",
        help_heading = "Connection"
    )]
    pub(crate) bind: String,
    #[arg(
        long,
        value_enum,
        default_value = "read-write",
        help = "Access mode: read-only|write-only|read-write",
        help_heading = "Connection"
    )]
    pub(crate) access: AccessModeCli,
    #[arg(
        long = "cors-origin",
        value_name = "ORIGIN",
        help = "Allow browser requests from this origin (repeatable, explicit list)",
        help_heading = "Connection"
    )]
    pub(crate) cors_origin: Vec<String>,
    #[arg(
        long,
        help = "Bearer token for auth (dev-only; prefer --token-file)",
        help_heading = "Authentication"
    )]
    pub(crate) token: Option<String>,
    #[arg(long, value_name = "PATH", help = "Read bearer token from file", value_hint = ValueHint::FilePath, help_heading = "Authentication")]
    pub(crate) token_file: Option<PathBuf>,
    #[arg(long, value_name = "PATH", help = "TLS certificate path (PEM; requires --tls-key)", value_hint = ValueHint::FilePath, help_heading = "TLS")]
    pub(crate) tls_cert: Option<PathBuf>,
    #[arg(long, value_name = "PATH", help = "TLS key path (PEM; requires --tls-cert)", value_hint = ValueHint::FilePath, help_heading = "TLS")]
    pub(crate) tls_key: Option<PathBuf>,
    #[arg(
        long,
        help = "Generate a self-signed TLS cert (conflicts with --tls-cert/--tls-key)",
        help_heading = "TLS"
    )]
    pub(crate) tls_self_signed: bool,
    #[arg(
        long,
        help = "Allow non-loopback binds (unsafe without TLS + token)",
        help_heading = "Safety"
    )]
    pub(crate) allow_non_loopback: bool,
    #[arg(
        long,
        help = "Allow non-loopback writes without TLS (unsafe)",
        help_heading = "Safety"
    )]
    pub(crate) insecure_no_tls: bool,
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_BODY_BYTES,
        help = "Max request body size in bytes (must be positive)",
        help_heading = "Safety"
    )]
    pub(crate) max_body_bytes: u64,
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_TAIL_TIMEOUT_MS,
        help = "Max tail timeout in milliseconds (must be positive)",
        help_heading = "Safety"
    )]
    pub(crate) max_tail_timeout_ms: u64,
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_TAIL_CONCURRENCY,
        help = "Max concurrent tail streams (must be positive)",
        help_heading = "Safety"
    )]
    pub(crate) max_tail_concurrency: usize,
}
