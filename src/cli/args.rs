//! Purpose: Define the complete clap argument and help contract.
//! Exports: `Cli`, command enums, and command argument structures.
//! Role: Parse syntax only; command execution belongs to sibling CLI modules.

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use clap_complete::aot::Shell;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "plasmite",
    version = env!("PLASMITE_BUILD_VERSION"),
    about = "Persistent JSON message pools for local and host-adjacent IPC",
    help_template = r#"{about-with-newline}
{before-help}USAGE
  {usage}

COMMANDS
  Send and read
    feed POOL [DATA | --file PATH]       Append JSON (omitted input uses stdin)
    follow POOL [--tail N | --since TIME]
                [--no-follow] [--replay SPEED]
                [--tag TAG] [--where EXPR] [--one] [--timeout DURATION]
    fetch POOL SEQ                      Read one message
    tap POOL -- COMMAND...              Capture command output
    duplex POOL [--me NAME]              Send and receive in one session

  Manage pools
    pool create NAME... [--size SIZE]    Create local pools
    pool list [SERVER]                   List local or remote pools
    pool info POOL                      Inspect capacity, bounds, and metrics
    pool delete POOL...                  Delete local pools
    doctor <POOL | --all>                Check local pool integrity

  Share and connect
    serve [SERVER]                      Run a server for this directory
    serve install [SERVER]              Install this server as a background service
    serve start|stop|restart|uninstall  Control the installed service
    serve logs [--follow]               Read or follow service logs
    serve status [--all]                List running servers or all saved setups
    access invite NAME                  Create a full-access directory key
    access keys                         List server-side keys
    access revoke ID                    Revoke a server-side key
    access connect SERVER               Verify a key and save a connection
    access list                         List saved server destinations
    access status SERVER                Check access, reachability, browser trust
    access disconnect SERVER            Forget saved credentials
    access untrust SHA256                Remove trust for one certificate

  Integrate and learn
    mcp [SERVER]                        Run Model Context Protocol on stdio
    completion SHELL                    Print shell completion code
    version                             Print the build version
    help [COMMAND...]                   Show root or command help

OPTIONS
{options}

{after-help}
"#,
    long_about = None,
    propagate_version = true,
    before_help = r#"A pool is a persistent, bounded stream that multiple processes can write and read.
Messages are JSON: `feed` appends, `follow` streams, and `fetch` reads one by sequence.

FIRST LOCAL WORKFLOW
  $ plasmite pool create chat
  $ plasmite follow chat                                      # Terminal 1
  $ plasmite feed chat '{"from":"alice","msg":"hello"}'       # Terminal 2

"#,
    after_help = r#"OUTPUT
  Human-readable by default. Use --json on reports and message commands for scripts.
  Structured output contains no color or commentary; errors go to stderr.

OPTIONS AND HELP
  Global options work before or after commands, up to tap's -- separator.
  One --dir selects pools and server-side keys, including mcp; conflicting repeats fail.
  $ plasmite follow chat --dir ./pools --tail 10
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
        global = true,
        action = clap::ArgAction::Append,
        help = "Directory for local pools and server-side keys (default: ~/.plasmite/pools)",
        value_hint = ValueHint::DirPath
    )]
    pub(crate) dir: Vec<PathBuf>,
    #[arg(
        long,
        global = true,
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
        display_order = 5,
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
        display_order = 0,
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
  - Receipts are human-readable by default; --json emits one receipt per line
  - --errors skip continues after bad records and exits 1 if any were rejected"#
    )]
    Feed {
        #[arg(long, help = "Emit structured JSON without color or commentary")]
        json: bool,
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
    },
    #[command(
        args_conflicts_with_subcommands = true,
        display_order = 7,
        about = "Share pools securely with named access keys",
        long_about = r#"Serve pools locally and share them over HTTPS with named access keys.

Run `plasmite access invite <name>` in another terminal to create a key."#,
        after_help = r#"EXAMPLES
  $ plasmite --dir ./pools serve
  $ plasmite serve install https://pools.example.com:9743
  $ plasmite serve status --all --json
  $ plasmite --dir ./pools access invite laptop

CONSTRAINTS
  - Request body, tail timeout, and tail concurrency limits must be positive
  - The local admin listener stays on loopback; remote clients use HTTPS"#
    )]
    Serve {
        #[command(subcommand)]
        command: Option<ServeSubcommand>,
        #[command(flatten)]
        run: Box<ServeRunArgs>,
    },
    #[command(
        display_order = 9,
        about = "Serve local or remote MCP tools and resources on stdio",
        long_about = r#"Start an MCP process on stdio.

With no server, the process exposes local pools. Pass a SERVER to connect to a
Plasmite server through this machine's saved native connection. Credentials stay
in Plasmite's access store and are reloaded before each remote request.

The process reads newline-delimited JSON-RPC requests from stdin and writes
responses to stdout. It exits when stdin closes."#,
        after_help = r#"EXAMPLES
  $ plasmite mcp
  $ plasmite mcp --dir /path/to/pools
  $ plasmite mcp https://pools.example.com:8443

INPUT AND OUTPUT
  Reads JSON-RPC on stdin and writes JSON-RPC on stdout until stdin closes.
  --dir selects local pools. A remote SERVER uses a saved connection and cannot be combined with --dir.
  The --remote option remains available for existing scripts."#
    )]
    Mcp {
        #[arg(
            value_name = "SERVER",
            conflicts_with = "remote",
            help = "Use a saved HTTPS connection to a Plasmite server"
        )]
        server: Option<String>,
        #[arg(
            long,
            value_name = "SERVER_URL",
            conflicts_with = "server",
            help = "Legacy spelling for SERVER"
        )]
        remote: Option<String>,
    },
    #[command(
        arg_required_else_help = true,
        display_order = 8,
        about = "Manage secure access to a shared pool directory",
        long_about = r#"Create access keys and connect to a shared pool server.

An access key grants access to every pool in the server’s pool directory.
Create a key with `invite` on the server machine. Use `connect` and `status` on a client machine.
`connect` asks for the key without displaying it."#
    )]
    Access {
        #[command(subcommand)]
        command: AccessSubcommand,
    },
    #[command(
        arg_required_else_help = true,
        display_order = 2,
        about = "Fetch one message by sequence number",
        long_about = r#"Fetch a message from a local name/path or remote pool URL. Human-readable by default; --json emits one message envelope."#,
        after_help = r#"EXAMPLES
  $ plasmite fetch foo 1
  $ plasmite fetch foo 42 --json | jq '.data'

INPUT AND OUTPUT
  Local names/paths and HTTP(S) pool URLs are accepted.
  Human-readable by default; --json emits one message envelope."#
    )]
    Fetch {
        #[arg(long, help = "Emit structured JSON without color or commentary")]
        json: bool,
        #[arg(help = "Pool ref: local name/path or HTTP(S) pool URL")]
        pool: String,
        #[arg(help = "Sequence number")]
        seq: u64,
    },
    #[command(
        arg_required_else_help = true,
        display_order = 1,
        about = "Follow messages from a pool",
        long_about = r#"Follow a pool and stream messages as they arrive.

By default, `follow` waits for new messages forever (Ctrl-C to stop).
Use `--tail N` to see recent history first, then keep following.
Use --no-follow with --tail or --since to read finite history.
Use --replay SPEED with history for local timed playback; it implies --no-follow."#,
        after_help = r#"EXAMPLES
  $ plasmite follow foo                                           # follow live
  $ plasmite follow foo --tail 10                                 # last 10 + live
  $ plasmite follow foo --where '.data.ok == true' --one          # match & exit
  $ plasmite follow foo --json | jq '.data'               # pipe to jq

LOCAL AND REMOTE
  Local and remote refs support --tail, --since, --no-follow, filters, and output.
  Remote refs reject --create, --replay, --no-notify, and --quiet-drops.
  --no-follow requires --tail or --since; finite history ends at the starting bound.
  --tail counts retained messages before filters. Filters apply to history and live output.
  Human-readable by default; --json emits one envelope per line.
  --jsonl and --format jsonl retain their meanings as compatibility aliases."#
    )]
    Follow {
        #[arg(
            long,
            help = "Read selected history and exit; requires --tail or --since"
        )]
        no_follow: bool,
        #[arg(long, help = "Emit structured JSON without color or commentary")]
        json: bool,
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
        #[arg(
            long,
            help = "Compatibility alias for structured JSON Lines output (prefer --json)"
        )]
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
            help = "Output format: pretty|jsonl (--json is the preferred structured-output flag)"
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
    },
    #[command(
        arg_required_else_help = true,
        display_order = 3,
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
  - Lines larger than the pool ring capacity fail; the wrapped child is terminated
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
        display_order = 4,
        about = "Send and follow from one command",
        long_about = r#"Read and write a pool from one process.

`duplex` follows a pool on stdout (like `follow`) while also sending input from stdin:

- TTY stdin: requires `--me`; each non-empty line appends a message with `.data = {"from": ME, "msg": LINE}`.
  Your own messages are hidden from output unless `--echo-self` is set.
- Non-TTY stdin: ingests stdin as a JSON stream (like `feed`, defaults: `--in auto --errors stop`).
  Duplex exits when stdin ends (EOF) or when the receive side ends (e.g. timeout/error).

Notes:
- Remote refs do not support --create. Both targets support --tail and --since."#,
        after_help = r#"INPUT AND EXIT
  - Terminal input requires --me and sends one chat message per non-empty line
  - Piped input is a JSON stream; connect before using a remote ref
  - Human-readable output by default; --json emits one message envelope per line
  - Exits 124 on timeout"#
    )]
    Duplex {
        #[arg(long, help = "Emit structured JSON without color or commentary")]
        json: bool,
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
        #[arg(
            long,
            help = "Compatibility alias for structured JSON Lines output (prefer --json)"
        )]
        jsonl: bool,
        #[arg(
            long,
            help = "Exit 124 if no output within duration (e.g. 500ms, 5s, 1m)"
        )]
        timeout: Option<String>,
        #[arg(
            long = "format",
            value_enum,
            help = "Output format: pretty|jsonl (--json is the preferred structured-output flag)"
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
        display_order = 6,
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
        display_order = 11,
        about = "Print version information",
        long_about = r#"Print human-readable version information by default.

Use --json for a stable machine-readable JSON report."#,
        after_help = r#"EXAMPLES
  $ plasmite version
  $ plasmite version --json | jq -r '.version'"#
    )]
    Version {
        #[arg(long, help = "Emit a JSON version report")]
        json: bool,
    },
    #[command(
        arg_required_else_help = true,
        display_order = 10,
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

#[derive(Debug, Subcommand)]
pub(crate) enum ServeSubcommand {
    #[command(about = "Install this server as a service that starts at boot")]
    Install {
        #[command(flatten)]
        run: Box<ServeRunArgs>,
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
    },
    #[command(about = "Start the installed background service")]
    Start {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
    },
    #[command(about = "Stop the background service")]
    Stop {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
    },
    #[command(about = "Restart the background service")]
    Restart {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
    },
    #[command(about = "Remove the background service")]
    Uninstall {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
    },
    #[command(about = "Show recent server logs")]
    Logs {
        #[arg(long, value_name = "N", help = "Number of recent lines to show")]
        tail: Option<usize>,
        #[arg(long, help = "Continue following new log lines")]
        follow: bool,
        #[arg(long, help = "Emit one JSON object per log line")]
        json: bool,
    },
    #[command(about = "Show running servers for this user")]
    Status {
        #[arg(long, help = "Include installed servers that are stopped or failed")]
        all: bool,
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum PoolCommand {
    #[command(
        arg_required_else_help = true,
        display_order = 0,
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
        display_order = 2,
        about = "Show pool metadata and bounds",
        long_about = r#"Show local or remote pool size, bounds, and metrics. Human-readable by default; --json emits a report."#,
        after_help = r#"EXAMPLES
  $ plasmite pool info foo
  $ plasmite pool info foo --json

TARGET AND OUTPUT
  Local names/paths and HTTP(S) pool URLs are accepted.
  Human-readable by default; --json emits one report."#
    )]
    Info {
        #[arg(help = "Pool ref: local name/path or HTTP(S) pool URL")]
        name: String,
        #[arg(long, help = "Emit JSON instead of human-readable output")]
        json: bool,
    },
    #[command(
        arg_required_else_help = true,
        display_order = 3,
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
        display_order = 1,
        about = "List local pools or a remote server directory",
        long_about = r#"List local pools or pools at the supplied HTTPS server origin.

Prints a human-readable table by default. Use --json for machine-readable output."#,
        after_help = r#"EXAMPLES
  $ plasmite pool list
  $ plasmite pool list --json
  $ plasmite pool list https://pools.example.com:9743 --json

NOTES
  - Human-readable output is the default.
  - Use --json for machine-readable output.
  - Non-.plasmite files are ignored.
  - Pools that cannot be read include an error field."#
    )]
    List {
        #[arg(
            value_name = "SERVER",
            help = "HTTPS server origin (omit for local pools)"
        )]
        server: Option<String>,
        #[arg(long, help = "Emit JSON instead of human-readable output")]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum AccessSubcommand {
    #[command(
        display_order = 0,
        about = "Create a named access key on the server machine",
        after_help = "Keys grant full access to the selected pool directory. Human-readable by default; --json emits a report containing the new key."
    )]
    Invite {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
        #[arg(
            value_name = "NAME",
            required_unless_present = "legacy_name",
            conflicts_with = "legacy_name",
            help = "Name to identify this client"
        )]
        name: Option<String>,
        #[arg(
            long = "name",
            value_name = "NAME",
            required_unless_present = "name",
            conflicts_with = "name",
            help = "Legacy spelling for the client name"
        )]
        legacy_name: Option<String>,
    },
    #[command(
        display_order = 3,
        about = "Connect this machine to a shared pool server",
        after_help = "Reads a key from a hidden terminal prompt or stdin, verifies it, and saves the connection for this OS user. Human-readable by default; --json emits a report.",
        long_about = "Verify the server URL and access key, then save the connection for future remote pool commands. The key is read from a hidden prompt or stdin. Reconnect with the same key to recover a lost local connection."
    )]
    Connect {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
        #[arg(value_name = "SERVER", help = "HTTPS address printed by the server")]
        url: String,
    },
    #[command(
        display_order = 5,
        about = "Show this machine's connection to a shared pool server",
        after_help = "Checks saved access, reachability, and browser trust without changing state. A completed check exits zero even when access fails. Human-readable by default; --json emits a report.",
        long_about = "Check saved access, reachability, and browser trust without changing state. A completed check exits zero even when access fails."
    )]
    Status {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
        #[arg(value_name = "SERVER", help = "HTTPS address printed by the server")]
        url: String,
    },
    #[command(
        display_order = 6,
        about = "Forget this machine's saved connection",
        after_help = "Forgets saved credentials for this OS user without contacting the server. Server permission remains until revoked. Human-readable by default; --json emits a report.",
        long_about = "Remove this machine's saved access key for a server. This does not revoke the key on the server and works while the server is offline."
    )]
    Disconnect {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
        #[arg(value_name = "SERVER", help = "HTTPS address of the saved connection")]
        url: String,
    },
    #[command(
        display_order = 7,
        about = "Remove one exact browser-trusted certificate",
        after_help = "Removes browser trust for one exact certificate; native credentials remain saved. Human-readable by default; --json emits a report.",
        long_about = "Remove the certificate identified by its SHA-256 fingerprint from this OS user's trust store. This works after the server renews its certificate or goes offline. Native credentials remain saved."
    )]
    Untrust {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
        #[arg(
            value_name = "SHA256",
            help = "Certificate fingerprint shown by access status"
        )]
        fingerprint: String,
    },
    #[command(
        display_order = 1,
        about = "List access keys on the server machine",
        after_help = "Lists server-side keys for the selected --dir. Human-readable by default; --json emits a report."
    )]
    Keys {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
    },
    #[command(
        display_order = 4,
        about = "List saved server destinations without contacting servers",
        after_help = "Reads this OS user's saved destinations without probing servers. Human-readable by default; --json emits an array."
    )]
    List {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
    },
    #[command(
        display_order = 2,
        about = "Revoke an access key on the server machine",
        after_help = "Withdraws server permission for the selected --dir. Human-readable by default; --json emits a report."
    )]
    Revoke {
        #[arg(long, help = "Emit a JSON report")]
        json: bool,
        #[arg(value_name = "ID", help = "Key ID shown by `access keys`")]
        id: String,
    },
}

#[derive(Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct ServeRunArgs {
    #[arg(
        value_name = "SERVER",
        conflicts_with = "shared_address",
        help = "Client-facing HTTPS origin"
    )]
    pub(crate) server: Option<String>,
    #[arg(
        long,
        value_name = "ADDRESS",
        help = "Local loopback bind address for credential-free administration",
        help_heading = "Connection"
    )]
    pub(crate) bind: Option<String>,
    #[arg(
        long = "remote-bind",
        value_name = "ADDRESS",
        help = "Numeric IP:port for authenticated HTTPS (bracket IPv6)",
        help_heading = "Connection"
    )]
    pub(crate) remote_bind: Option<String>,
    #[arg(
        long = "shared-address",
        value_name = "URL",
        conflicts_with = "server",
        help = "Legacy public HTTPS URL option",
        help_heading = "Connection"
    )]
    pub(crate) shared_address: Option<String>,
    #[arg(
        long = "front-cert",
        value_name = "PATH",
        help = "Certificate used by an HTTPS-terminating proxy (sets the expected remote identity)",
        value_hint = ValueHint::FilePath,
        help_heading = "TLS"
    )]
    pub(crate) front_cert: Option<PathBuf>,
    #[arg(long, value_name = "PATH", help = "Backend TLS certificate path (PEM; requires --tls-key)", value_hint = ValueHint::FilePath, help_heading = "TLS")]
    pub(crate) tls_cert: Option<PathBuf>,
    #[arg(long, value_name = "PATH", help = "Backend TLS key path (PEM; requires --tls-cert)", value_hint = ValueHint::FilePath, help_heading = "TLS")]
    pub(crate) tls_key: Option<PathBuf>,
    #[arg(
        long,
        value_name = "BYTES",
        help = "Max request body size in bytes (must be positive)",
        help_heading = "Safety"
    )]
    pub(crate) max_body_bytes: Option<u64>,
    #[arg(
        long,
        value_name = "MILLISECONDS",
        help = "Max tail timeout in milliseconds (must be positive)",
        help_heading = "Safety"
    )]
    pub(crate) max_tail_timeout_ms: Option<u64>,
    #[arg(
        long,
        value_name = "N",
        help = "Max concurrent tail streams (must be positive)",
        help_heading = "Safety"
    )]
    pub(crate) max_tail_concurrency: Option<usize>,
}

impl Command {
    pub(crate) fn json_output(&self) -> bool {
        match self {
            Self::Version { json }
            | Self::Doctor { json, .. }
            | Self::Feed { json, .. }
            | Self::Fetch { json, .. } => *json,
            Self::Follow {
                json,
                jsonl,
                format,
                ..
            }
            | Self::Duplex {
                json,
                jsonl,
                format,
                ..
            } => *json || *jsonl || matches!(format, Some(FollowFormat::Jsonl)),
            Self::Pool { command } => match command {
                PoolCommand::Create { json, .. }
                | PoolCommand::Info { json, .. }
                | PoolCommand::Delete { json, .. }
                | PoolCommand::List { json, .. } => *json,
            },
            Self::Access { command } => match command {
                AccessSubcommand::Invite { json, .. }
                | AccessSubcommand::Connect { json, .. }
                | AccessSubcommand::Status { json, .. }
                | AccessSubcommand::Disconnect { json, .. }
                | AccessSubcommand::Untrust { json, .. }
                | AccessSubcommand::Keys { json }
                | AccessSubcommand::List { json }
                | AccessSubcommand::Revoke { json, .. } => *json,
            },
            Self::Serve {
                command: Some(command),
                ..
            } => match command {
                ServeSubcommand::Install { json, .. }
                | ServeSubcommand::Start { json }
                | ServeSubcommand::Stop { json }
                | ServeSubcommand::Restart { json }
                | ServeSubcommand::Uninstall { json }
                | ServeSubcommand::Logs { json, .. }
                | ServeSubcommand::Status { json, .. } => *json,
            },
            _ => false,
        }
    }
}
