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
    feed POOL [MESSAGE | --file PATH]    Append complete messages (stdin when omitted)
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
    mcp                                 Run local Model Context Protocol on stdin/stdout
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
  $ plasmite feed chat --data '{"from":"alice","msg":"hello"}'       # Terminal 2

"#,
    after_help = r#"OUTPUT
  Readable in a terminal; compact JSON in a pipe or file.
  Use --format pretty|json|yaml|lite3|lite3-raw to choose explicitly.
  feed, fetch, follow, and duplex write complete messages. Pipes preserve data and tags.
  YAML marks each document with --- and ...; Lite3 marks each binary message with its length.
  Errors and notices go to stderr. Help and completion stay text.

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
    #[arg(
        long,
        global = true,
        value_enum,
        help = "Output: pretty|json|yaml|lite3|lite3-raw (default: pretty in a terminal, json in a pipe)"
    )]
    pub(crate) format: Option<super::output::OutputMode>,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub(crate) enum ColorMode {
    Auto,
    Always,
    Never,
}

pub(crate) use super::message_stream::InputFormat as InputMode;

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
        long_about = r#"Send JSON or pre-encoded Lite3 messages to a pool.

Accepts local pool refs (name/path), HTTP(S) refs, and udp://host:port/<pool>,
inline JSON, file input (-f/--file), or streams via stdin (auto-detected).
Use --input-format lite3-raw for one raw attachment from a file or stdin."#,
        after_help = r#"EXAMPLES
  $ plasmite feed foo --data '{"hello":"world"}'             # create a message
  $ plasmite feed foo --data --tag alert '{"msg":"check"}'  # create with a tag
  $ plasmite follow source | plasmite feed destination       # copy messages
  $ plasmite feed foo < messages.jsonl                      # reload saved messages

INPUT AND OUTPUT
  - Complete messages contain object data and optional meta.tags; missing tags mean []
  - Use --data to construct messages from application objects; --tag requires --data
  - Choose one input source: MESSAGE, --file, or stdin
  - JSON, marked YAML, and framed Lite3 detect automatically and append one message at a time
  - --input-format chooses an explicit encoding; lite3-raw reads one attachment through EOF
  - Each append gets a new sequence and time and writes the complete destination message
  - UDP sends produce no storage receipt and may be lost, duplicated, or reordered
  - Invalid input stops immediately; earlier successful appends remain stored
  - stdout holds messages; stderr holds errors and notices"#
    )]
    Feed {
        #[arg(help = "Pool name/path or http(s)://host:port/<pool> or udp://host:port/<pool>")]
        pool: String,
        #[arg(help = "Complete message, or application object with --data")]
        data: Option<String>,
        #[arg(
            long = "data",
            help = "Create messages from application objects instead of reading complete messages"
        )]
        contents: bool,
        #[arg(
            long,
            requires = "contents",
            help = "Tag for messages created with --data (repeatable)"
        )]
        tag: Vec<String>,
        #[arg(
            short = 'f',
            long = "file",
            help = "Message file (JSON, YAML, or Lite3; use - for stdin)",
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
        #[arg(long, default_value_t = 0, help = "Retry count for Busy errors")]
        retry: u32,
        #[arg(
            long,
            help = "Delay between retries (e.g. 50ms, 1s, 2m; requires --retry > 0)"
        )]
        retry_delay: Option<String>,
        #[arg(
            short = 'i',
            long = "input-format",
            default_value = "auto",
            value_enum,
            help = "Input mode for file or stdin streams",
            long_help = "Input encoding: auto recognizes JSON objects, marked YAML documents, and framed Lite3. JSON accepts whitespace-separated objects. YAML accepts marked documents or one unmarked document through EOF. lite3-raw reads one attachment through EOF."
        )]
        input: InputMode,
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
  $ plasmite serve status --all --format json
  $ plasmite --dir ./pools access invite laptop

CONSTRAINTS
  - Request body, tail timeout, and tail concurrency limits must be positive
  - The local admin listener stays on loopback; remote clients use HTTPS
  - Installed servers start at boot
  - --udp-follow enables unauthenticated, unencrypted UDP reads and implies UDP input
  - On Windows, install, update, and uninstall need approval from the pool-owning administrator"#
    )]
    Serve {
        #[command(subcommand)]
        command: Option<ServeSubcommand>,
        #[command(flatten)]
        run: Box<ServeRunArgs>,
    },
    #[command(
        display_order = 9,
        about = "Serve local MCP tools and resources on stdin/stdout",
        long_about = r#"Start an MCP process that exposes local pools.

The process reads newline-delimited JSON-RPC requests from stdin and writes
responses to stdout. It exits when stdin closes. Use `--dir` to select the
local pool directory.

For a remote server, configure your MCP client with its HTTPS /mcp URL."#,
        after_help = r#"EXAMPLES
  $ plasmite mcp
  $ plasmite mcp --dir /path/to/pools

INPUT AND OUTPUT
  Reads JSON-RPC on stdin and writes JSON-RPC on stdout until stdin closes."#
    )]
    Mcp,
    #[command(
        arg_required_else_help = true,
        display_order = 8,
        about = "Manage secure access to a shared pool directory",
        long_about = r#"Create access keys and connect to a shared pool server.

An access key grants access to every pool in the server’s pool directory.
Create a key with `invite` on the server machine. For Plasmite CLI access, use
`connect` and `status` on the client machine. Remote MCP clients connect directly
to the server's HTTPS /mcp URL and authorize through OAuth.
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
        long_about = r#"Fetch a message from a local name/path or remote pool URL.
Readable in a terminal; compact JSON in a pipe; --format json or --format yaml emits one message envelope.
YAML uses explicit document start and end markers.
Use --format lite3 for the original binary message bytes."#,
        after_help = r#"EXAMPLES
  $ plasmite fetch foo 7
  $ plasmite fetch foo 7 | plasmite feed bar
  $ plasmite fetch foo 7 --format yaml
  $ plasmite fetch foo 7 --format lite3-raw > heartbeat.lite3

OUTPUT
  Writes one complete message; --format selects the same encoding as follow and feed.
  lite3 writes a framed message; lite3-raw writes only the original attachment bytes.
  Local and remote reads share these formats. Errors go to stderr."#
    )]
    Fetch {
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
  $ plasmite follow foo                                      # new messages
  $ plasmite follow foo --tail 10                            # history, then new messages
  $ plasmite follow foo --where '.data.ok == true' --one      # match and exit
  $ plasmite follow source | jq -c '.data.temperature += 1' | plasmite feed destination
  $ plasmite follow foo --tail 100 --no-follow > messages.jsonl

LOCAL AND REMOTE
  Local, HTTP(S), and UDP refs support history, filters, and JSON, YAML, or framed Lite3.
  HTTP(S) refs reject --create, --replay, --no-notify, and --quiet-drops.
  UDP refs reject --create, --replay, and --no-notify; UDP gaps appear on stderr.
  --no-follow requires --tail or --since and stops at the starting history bound.
  --tail counts retained messages before filters; filters apply to history and new messages.
  Finite reads exit nonzero if retention removes selected messages.
  Live reads report dropped messages on stderr and continue."#
    )]
    Follow {
        #[arg(
            long,
            help = "Read selected history and exit; requires --tail or --since"
        )]
        no_follow: bool,
        #[arg(help = "Pool name/path or http(s)://host:port/<pool> or udp://host:port/<pool>")]
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
            help = "Exit 124 if no output within duration (e.g. 500ms, 5s, 1m)"
        )]
        timeout: Option<String>,
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
- Non-TTY stdin: ingests stdin as a JSON stream (like `feed`, defaults: automatic input detection).
  Duplex exits when stdin ends (EOF) or when the receive side ends (e.g. timeout/error).

Notes:
- Remote refs do not support --create. Both targets support --tail and --since."#,
        after_help = r#"EXAMPLES
  $ plasmite duplex chat --me alice
  $ plasmite follow source | plasmite duplex destination

INPUT AND OUTPUT
  Terminal input creates chat messages and requires --me.
  Piped stdin accepts complete JSON, marked YAML, or framed Lite3 messages.
  stdout streams received complete messages; stderr holds errors and notices.
  stdin EOF ends the session; use feed to copy a finite stream and echo every appended message.
  --echo-self includes received messages whose from field matches --me."#
    )]
    Duplex {
        #[arg(help = "Pool name/path or http(s)://host:port/<pool> or udp://host:port/<pool>")]
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
            help = "Exit 124 if no output within duration (e.g. 500ms, 5s, 1m)"
        )]
        timeout: Option<String>,
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
  $ plasmite doctor --all --format json

NOTES
  - Readable output in a terminal; compact JSON in a pipe or file.
  - Use --format json or --format yaml for machine-readable output.
  - Exits nonzero when corruption is detected."#
    )]
    Doctor {
        #[arg(help = "Pool name or path", required = false)]
        pool: Option<String>,
        #[arg(long, help = "Validate all pools in the pool directory")]
        all: bool,
    },
    #[command(
        display_order = 11,
        about = "Print version information",
        long_about = r#"Print version information: readable output in a terminal, compact JSON in a pipe or file.

Use --format json for JSON or --format yaml for the same report as YAML."#,
        after_help = r#"EXAMPLES
  $ plasmite version
  $ plasmite version --format json | jq -r '.version'
  $ plasmite version --format yaml"#
    )]
    Version {},
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
        #[arg(
            long,
            conflicts_with = "udp",
            help = "Remove UDP input from the installed server"
        )]
        no_udp: bool,
        #[arg(
            long,
            conflicts_with = "udp_follow",
            help = "Remove UDP reads from the installed server while retaining UDP input"
        )]
        no_udp_follow: bool,
    },
    #[command(about = "Start the installed background service")]
    Start {},
    #[command(about = "Stop the background service")]
    Stop {},
    #[command(about = "Restart the background service")]
    Restart {},
    #[command(about = "Remove the background service")]
    Uninstall {},
    #[command(about = "Show recent server logs")]
    Logs {
        #[arg(long, value_name = "N", help = "Number of recent lines to show")]
        tail: Option<usize>,
        #[arg(long, help = "Continue following new log lines")]
        follow: bool,
    },
    #[command(about = "Show running servers for this user")]
    Status {
        #[arg(long, help = "Include installed servers that are stopped or failed")]
        all: bool,
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
  $ plasmite pool create --format json foo

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
    },
    #[command(
        arg_required_else_help = true,
        display_order = 2,
        about = "Show pool metadata and bounds",
        long_about = r#"Show local or remote pool size, bounds, and metrics. Readable in a terminal; compact JSON in a pipe; --format json emits JSON and --format yaml emits the same report as YAML."#,
        after_help = r#"EXAMPLES
  $ plasmite pool info foo
  $ plasmite pool info foo --format json

TARGET AND OUTPUT
  Local names/paths and HTTP(S) pool URLs are accepted.
  Readable in a terminal; compact JSON in a pipe; --format json emits one report."#
    )]
    Info {
        #[arg(help = "Pool ref: local name/path or HTTP(S) pool URL")]
        name: String,
    },
    #[command(
        arg_required_else_help = true,
        display_order = 3,
        about = "Delete one or more pool files",
        long_about = r#"Delete one or more pool files (destructive, cannot be undone)."#,
        after_help = r#"EXAMPLES
  $ plasmite pool delete foo
  $ plasmite pool delete foo bar baz
  $ plasmite pool delete --format json foo bar

NOTES
  - Readable output in a terminal; compact JSON in a pipe or file.
  - Use --format json or --format yaml for machine-readable output.
  - Best effort: attempts all deletes and reports per-pool failures.
  - Exits non-zero if any requested pool failed to delete."#
    )]
    Delete {
        #[arg(required = true, help = "Pool name(s) or path(s)")]
        names: Vec<String>,
    },
    #[command(
        display_order = 1,
        about = "List local pools or a remote server directory",
        long_about = r#"List local pools or pools at the supplied HTTPS server origin.

Prints a readable table in a terminal or compact JSON in a pipe or file. Use --format to choose another output format."#,
        after_help = r#"EXAMPLES
  $ plasmite pool list
  $ plasmite pool list --format json
  $ plasmite pool list https://pools.example.com:9743 --format json

NOTES
  - Readable output in a terminal; compact JSON in a pipe or file.
  - Use --format json or --format yaml for machine-readable output.
  - Non-.plasmite files are ignored.
  - Pools that cannot be read include an error field."#
    )]
    List {
        #[arg(
            value_name = "SERVER",
            help = "HTTPS server origin (omit for local pools)"
        )]
        server: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum AccessSubcommand {
    #[command(
        display_order = 0,
        about = "Create a named access key on the server machine",
        after_help = "Keys grant full access to the selected pool directory. Readable in a terminal; compact JSON in a pipe; --format json emits JSON and --format yaml emits the same report, including the new key, as YAML."
    )]
    Invite {
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
        after_help = "Reads a key from a hidden terminal prompt or stdin, verifies it, and saves the connection for this OS user. Readable in a terminal; compact JSON in a pipe; --format json emits JSON and --format yaml emits the same report as YAML.",
        long_about = "Verify the server URL and access key, then save the connection for future remote pool commands. The key is read from a hidden prompt or stdin. Reconnect with the same key to recover a lost local connection."
    )]
    Connect {
        #[arg(value_name = "SERVER", help = "HTTPS address printed by the server")]
        url: String,
    },
    #[command(
        display_order = 5,
        about = "Show this machine's connection to a shared pool server",
        after_help = "Checks saved access, reachability, and browser trust without changing state. A failed reachability or access check exits nonzero. Readable in a terminal; compact JSON in a pipe; --format json emits JSON and --format yaml emits the same report as YAML.",
        long_about = "Check saved access, reachability, and browser trust without changing state. A failed reachability or access check exits nonzero."
    )]
    Status {
        #[arg(value_name = "SERVER", help = "HTTPS address printed by the server")]
        url: String,
    },
    #[command(
        display_order = 6,
        about = "Forget this machine's saved connection",
        after_help = "Forgets saved credentials for this OS user without contacting the server. Server permission remains until revoked. Readable in a terminal; compact JSON in a pipe; --format json emits JSON and --format yaml emits the same report as YAML.",
        long_about = "Remove this machine's saved access key for a server. This does not revoke the key on the server and works while the server is offline."
    )]
    Disconnect {
        #[arg(value_name = "SERVER", help = "HTTPS address of the saved connection")]
        url: String,
    },
    #[command(
        display_order = 7,
        about = "Remove one exact browser-trusted certificate",
        after_help = "Removes browser trust for one exact certificate; native credentials remain saved. Readable in a terminal; compact JSON in a pipe; --format json emits JSON and --format yaml emits the same report as YAML.",
        long_about = "Remove the certificate identified by its SHA-256 fingerprint from this OS user's trust store. This works after the server renews its certificate or goes offline. Native credentials remain saved."
    )]
    Untrust {
        #[arg(
            value_name = "SHA256",
            help = "Certificate fingerprint shown by access status"
        )]
        fingerprint: String,
    },
    #[command(
        display_order = 1,
        about = "List access keys on the server machine",
        after_help = "Lists server-side keys for the selected --dir. Readable in a terminal; compact JSON in a pipe; --format json emits JSON and --format yaml emits the same report as YAML."
    )]
    Keys {},
    #[command(
        display_order = 4,
        about = "List saved server destinations without contacting servers",
        after_help = "Reads this OS user's saved destinations without probing servers. Readable in a terminal; compact JSON in a pipe; --format json emits JSON and --format yaml emits the same array as YAML."
    )]
    List {},
    #[command(
        display_order = 2,
        about = "Revoke an access key on the server machine",
        after_help = "Withdraws server permission for the selected --dir. Readable in a terminal; compact JSON in a pipe; --format json emits JSON and --format yaml emits the same report as YAML."
    )]
    Revoke {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(
        long,
        value_name = "ADDRESS",
        num_args = 0..=1,
        default_missing_value = "127.0.0.1:9701",
        require_equals = true,
        help = "UDP append input on numeric IP:port (default 127.0.0.1:9701; max 65507 bytes including routing; delivery may lose, duplicate, or reorder messages; no receipt, authentication, or encryption)",
        help_heading = "Connection"
    )]
    pub(crate) udp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(long = "udp-follow", action = clap::ArgAction::SetTrue, help = "Allow UDP followers (unauthenticated and unencrypted; implies UDP input)", help_heading = "Connection")]
    pub(crate) udp_follow: Option<bool>,
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
