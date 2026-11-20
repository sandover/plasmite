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
    feed POOL [MESSAGE]                 Send a JSON object, or messages from stdin
    follow POOL [--since POSITION | --last N] [--tag TAG] [--one]
                                        Stream new messages by default
    fetch POOL [SEQ | --since POSITION | --last N]
                                        Read retained messages and exit
    duplex POOL                         Send and receive in one session

  Manage pools
    pool create NAME... [--size SIZE]   Create local pools
    pool list [SERVER]                  List local or remote pools
    pool info POOL                      Inspect capacity, bounds, and metrics
    pool delete POOL...                 Delete local pools
    doctor <POOL | --all>               Check local pool integrity

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
    access untrust SHA256               Remove trust for one certificate

  Integrate and learn
    mcp                                 Run local Model Context Protocol on stdin/stdout
    completion SHELL                    Print shell completion code
    version                             Print the build version
    help [COMMAND...]                   Show root or command help

OPTIONS
{options}{after-help}
"#,
    long_about = None,
    propagate_version = true,
    before_help = r#"A pool is a pipe that remembers. It keeps a fixed amount of history and drops
the oldest messages as it fills. Feed adds a message, follow listens for new
messages, and fetch reads retained history or one sequence. Use --since to
start at oldest, a sequence, a time, or a duration. Use --last to start N
messages back.

FIRST LOCAL WORKFLOW
  $ plasmite pool create chat
  $ plasmite follow chat                                      # Terminal 1
  $ plasmite feed chat '{"from":"alice","msg":"hello"}'       # Terminal 2"#,
    after_help = r#"OUTPUT
  Readable in a terminal; compact JSON in a pipe or file.
  Use --format pretty|json|yaml|lite3|lite3-raw to choose explicitly.
  feed, fetch, follow, and duplex write complete messages. Pipes preserve data and tags.
  YAML marks each document with --- and ...; Lite3 marks each binary message with its length.
  Errors and notices go to stderr. Help and completion stay text.

OPTIONS AND HELP
  Global options work before or after commands.
  One --dir selects pools and server-side keys, including mcp; conflicting repeats fail.
  $ plasmite follow chat --dir ./pools --last 10
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
        help_heading = "Global options",
        help = "Directory for local pools and server-side keys (default: ~/.plasmite/pools)",
        value_hint = ValueHint::DirPath
    )]
    pub(crate) dir: Vec<PathBuf>,
    #[arg(
        long,
        global = true,
        default_value = "auto",
        value_enum,
        help_heading = "Global options",
        help = "Colorize stderr diagnostics and pretty JSON output"
    )]
    pub(crate) color: ColorMode,
    #[arg(
        long,
        global = true,
        value_enum,
        help_heading = "Global options",
        help = "Output format (default: pretty in a terminal, json in a pipe)"
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
        about = "Create, list, inspect, and delete pools",
        long_about = r#"Create, list, inspect, and delete pool files.

A pool is a bounded ring buffer on disk. Many processes can write and read it at
once, and it drops the oldest messages when it fills."#,
        after_help = r#"EXAMPLES
  $ plasmite pool create chat
  $ plasmite pool create --size 8M builds sensors
  $ plasmite pool info chat
  $ plasmite pool list
  $ plasmite pool delete chat
  $ plasmite pool delete builds sensors

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
        about = "Send JSON messages to a pool",
        long_about = r#"Send JSON messages to a pool.

Give one JSON object as an argument, or pipe messages on stdin. Stdin accepts
JSON, marked YAML, and framed Lite3, detected automatically."#,
        after_help = r#"EXAMPLES
  $ plasmite feed sensors '{"temp_c": 21.4}'                  # send one object
  $ plasmite feed builds --tag ci '{"step":"test","status":"ok"}'
  $ plasmite follow builds | plasmite feed archive --create   # copy messages
  $ plasmite feed events < messages.jsonl                     # reload saved messages

INPUT AND OUTPUT
  - Objects are application data unless they have exactly seq, time, meta.tags, and data
  - Complete messages preserve data and tags; source seq/time are validated and replaced
  - --tag adds tags to application objects; complete messages supply their own tags
  - Choose one input source: a message argument or stdin
  - JSON, marked YAML, and framed Lite3 detect automatically and append one message at a time
  - --input-format lines stores each input line as a message with a line field
  - --input-format lite3-raw reads one attachment through EOF
  - Each append gets a new sequence and time and writes the complete destination message
  - UDP sends produce no storage receipt and may be lost, duplicated, or reordered
  - Invalid input stops immediately; earlier successful appends remain stored
  - stdout holds messages; stderr holds errors and notices"#
    )]
    Feed {
        #[arg(help = "Pool name/path or http(s)://host:port/<pool> or udp://host:port/<pool>")]
        pool: String,
        #[arg(help = "Application object or complete message")]
        data: Option<String>,
        #[arg(long, help = "Tag for application objects (repeatable)")]
        tag: Vec<String>,
        #[arg(long, default_value = "fast", help = "Durability mode: fast|flush")]
        durability: String,
        #[arg(long, help = "Create the pool if it is missing")]
        create: bool,
        #[arg(
            short = 'i',
            long = "input-format",
            default_value = "auto",
            value_enum,
            help = "Input mode for stdin streams",
            long_help = "Input encoding: auto recognizes JSON objects, marked YAML documents, and framed Lite3. JSON accepts whitespace-separated objects. YAML accepts marked documents or one unmarked document through EOF. lines stores each input line as a message with a line field. lite3-raw reads one attachment through EOF."
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
  $ plasmite serve install https://pools.example.net:9743
  $ plasmite serve status --all --format json
  $ plasmite --dir ./pools access invite laptop

CONSTRAINTS
  - Request body, follow timeout, and follow stream limits must be positive
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
        about = "Read retained messages and exit",
        long_about = r#"Read a pool's retained history as it stood when the read opened.
Use --since or --last to choose a starting position, and --tag to select messages.
--last counts messages before the tag filter: `--last 10 --tag error` starts 10
messages back, then shows only those tagged error.
Give a sequence as a positional argument to fetch exactly one message."#,
        after_help = r#"EXAMPLES
  $ plasmite fetch builds 7
  $ plasmite fetch builds --last 10 --tag error
  $ plasmite fetch builds --since 5m
  $ plasmite fetch builds 7 | plasmite feed archive --create
  $ plasmite fetch builds 7 --format yaml
  $ plasmite fetch builds 7 --format lite3-raw > build.lite3

OUTPUT
  Writes complete messages; --format selects the same encoding as follow and feed.
  lite3 writes framed messages; lite3-raw requires an exact sequence.
  Local and remote reads share these formats. Errors go to stderr."#
    )]
    Fetch {
        #[arg(help = "Pool name/path or http(s)://host:port/<pool> or udp://host:port/<pool>")]
        pool: String,
        #[arg(help = "Fetch this exact sequence", conflicts_with_all = ["since", "last", "tags"])]
        seq: Option<u64>,
        #[arg(
            long,
            help = "Start at a sequence, oldest, an RFC 3339 time, or a duration"
        )]
        since: Option<String>,
        #[arg(
            long,
            short = 'n',
            conflicts_with = "since",
            help = "Start N messages back"
        )]
        last: Option<u64>,
        #[arg(long = "tag", help = "Require a tag (repeatable)")]
        tags: Vec<String>,
    },
    #[command(
        arg_required_else_help = true,
        display_order = 1,
        about = "Follow messages from a pool",
        long_about = r#"Follow a pool and stream messages as they arrive.

By default, follow waits for new messages. Use --since or --last to begin with
retained history. Use --one to wait for one matching message and exit."#,
        after_help = r#"EXAMPLES
  $ plasmite follow builds
  $ plasmite follow builds --last 10 --tag error
  $ plasmite follow builds --tag deploy --one
  $ plasmite follow builds | plasmite feed archive --create

LOCAL AND REMOTE
  Use --create to open a missing local or HTTPS pool at the default size.
  --last counts messages before the tag filter: `--last 10 --tag error` starts
  10 messages back, then shows only those tagged error.
  Use fetch to read retained messages and exit.
  Live reads report retention gaps on stderr and continue.
  UDP delivers messages in arrival order, including late messages and duplicates."#
    )]
    Follow {
        #[arg(help = "Pool name/path or http(s)://host:port/<pool> or udp://host:port/<pool>")]
        pool: String,
        #[arg(long, help = "Create the pool if missing")]
        create: bool,
        #[arg(
            long,
            short = 'n',
            conflicts_with = "since",
            help = "Start N messages back"
        )]
        last: Option<u64>,
        #[arg(long, help = "Exit after emitting one matching message")]
        one: bool,
        #[arg(
            long,
            help = "Start at a sequence, oldest, an RFC 3339 time, or a duration"
        )]
        since: Option<String>,
        #[arg(long = "tag", value_name = "TAG", help = "Require a tag (repeatable)")]
        tags: Vec<String>,
    },
    #[command(
        arg_required_else_help = true,
        display_order = 4,
        about = "Send and follow from one command",
        long_about = r#"Send messages from stdin and receive complete messages on stdout.

Terminal and piped stdin accept the same JSON, marked YAML, and framed Lite3
messages as feed. Duplex assigns no meaning to fields inside data.

Input EOF closes sending. Reception continues until you stop the command or
close its output pipe."#,
        after_help = r#"EXAMPLES
  $ plasmite duplex events --create

INPUT AND OUTPUT
  JSON, marked YAML, and framed Lite3 use the same rules as feed.
  stdout streams received complete messages, including your own appends.
  Input EOF closes sending while reception continues.
  UDP refs do not support --create."#
    )]
    Duplex {
        #[arg(help = "Pool name/path or http(s)://host:port/<pool> or udp://host:port/<pool>")]
        pool: String,
        #[arg(long, help = "Create a missing local or HTTP(S) pool")]
        create: bool,
        #[arg(long = "last", short = 'n', help = "Print the last N messages first")]
        last: Option<u64>,
        #[arg(
            long,
            help = "Start at a sequence, oldest, an RFC 3339 time, or a duration",
            conflicts_with = "last"
        )]
        since: Option<String>,
    },
    #[command(
        arg_required_else_help = true,
        display_order = 6,
        about = "Diagnose pool health",
        override_usage = "plasmite doctor [OPTIONS] <POOL|--all>",
        long_about = r#"Validate one pool (or all pools) and emit a diagnostic report."#,
        after_help = r#"EXAMPLES
  $ plasmite doctor chat
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

Pools include an inline sequence index by default for fast `fetch POOL SEQ` lookups."#,
        after_help = r#"EXAMPLES
  $ plasmite pool create chat
  $ plasmite pool create --size 8M builds sensors events
  $ plasmite pool create --size 8M --index-capacity 4096 builds
  $ plasmite pool create --format json chat

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
  $ plasmite pool info chat
  $ plasmite pool info chat --format json

TARGET AND OUTPUT
  Local names/paths and HTTP(S) pool URLs are accepted.
  Readable in a terminal; compact JSON in a pipe; --format json emits one report."#
    )]
    Info {
        #[arg(help = "Pool name/path or http(s)://host:port/<pool>")]
        name: String,
    },
    #[command(
        arg_required_else_help = true,
        display_order = 3,
        about = "Delete one or more pool files",
        long_about = r#"Delete one or more pool files (destructive, cannot be undone)."#,
        after_help = r#"EXAMPLES
  $ plasmite pool delete chat
  $ plasmite pool delete builds sensors events
  $ plasmite pool delete --format json builds sensors

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
  $ plasmite pool list https://pools.example.net:9743 --format json

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
            hide = true
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
        hide = true
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
        help = "Timeout in milliseconds for opening a read, completing a fetch response, or a stalled follow (must be positive)",
        help_heading = "Safety"
    )]
    pub(crate) max_tail_timeout_ms: Option<u64>,
    #[arg(
        long,
        value_name = "N",
        help = "Max concurrent live reads: HTTP follows, MCP follow calls and event streams (default 64)",
        help_heading = "Safety"
    )]
    pub(crate) max_tail_concurrency: Option<usize>,
}
