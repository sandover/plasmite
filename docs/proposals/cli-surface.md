# Proposed CLI surface

Historical design proposal. Current command behavior is documented in the
[CLI guide](../cli.md) and [CLI contract](../../spec/v0/SPEC.md).

A pool is a persistent, bounded stream of JSON messages. Multiple processes
can append and read independently. Local use requires no server. Remote use
names a server explicitly and uses saved credentials for that destination.

## Commands

Signatures show the main choices. Command help lists every option. `POOL` means
a pool reference; the target rules below define its accepted forms. `SERVER`
means an HTTPS origin such as `https://host:9743`. Reports and message commands
share the `--json` option described below.

```text
plasmite [global options] <command> [arguments]

Send and read
  feed POOL [DATA | --file PATH]            append JSON; stdin supplies omitted input
  follow POOL [--tail N | --since TIME]     read messages, then wait for new ones
              [--no-follow] [--replay SPEED]
              [--tag TAG] [--where EXPR] [--one] [--timeout DURATION]
  fetch POOL SEQ                            read one message by sequence number
  tap POOL -- COMMAND...                    run a command and capture its output
  duplex POOL [--me NAME]                   send and receive in one session

Manage pools
  pool create NAME... [--size SIZE]         create local pools
  pool list [SERVER]                        list local pools or a remote server's pools
  pool info POOL                            inspect capacity, retained bounds, and metrics
  pool delete POOL...                       delete local pools
  doctor <POOL | --all>                     check local pool integrity

Share and connect
  serve [SERVER]                            run a server for the selected pool directory
  serve install [SERVER]                    start it and arrange boot startup
  serve start|stop|restart|uninstall        manage its installed server
  serve logs                                read the installed server log
  serve status [--all]                      list live servers; include installed setups with --all
  access invite NAME                        create a full-access key for this directory
  access keys                               list this directory's server-side keys
  access revoke ID                          revoke a server-side key
  access connect SERVER                     verify a key and save a connection
  access list                               list this user's saved server destinations
  access status SERVER                      check saved access, reachability, and browser trust
  access disconnect SERVER                  forget saved credentials
  access untrust SHA256                     remove trust for one exact certificate

Integrate and learn
  mcp                                      run Model Context Protocol over stdin/stdout
  completion SHELL                          print a shell completion script
  version                                   print the build version
  help [COMMAND...]                         show root or command help
```

## Shared rules

- **Global options:** `--dir DIR`, `--color auto|always|never`, `--help`, and
  `--version`. Accept them before or after the command, up to `tap`'s `--`
  separator. One `--dir` selects local pools and server-side keys for the whole
  invocation, including `mcp`; reject conflicting repeats. Saved connections
  and server discovery belong to the current OS user.
- **Output:** Human-readable by default. `--json` selects structured output
  explicitly: one JSON document for a report, one JSON message envelope per
  line for reads, and one receipt per line for appends. Terminal detection
  controls presentation. Structured stdout contains no color or commentary.
  This policy covers `feed`, `follow`, `fetch`, `duplex`, `pool` subcommands,
  `doctor`, `serve status`, `access` subcommands, and `version`.
  `mcp` always emits JSON-RPC; `completion` always emits shell code. `tap`
  passes through child output; its existing `--quiet` suppresses that output.
- **Targets:** `feed`, `follow`, `fetch`, `duplex`, and `pool info` accept local
  names/paths and HTTPS pool URLs. Loopback HTTP pool URLs address the local
  listener. `pool list SERVER` lists a remote directory. `pool create`,
  `pool delete`, `tap`, and `doctor` operate locally. `--create` and its size
  option remain local conveniences for messaging commands that support them.
- **Serving:** Positional `serve SERVER` names the public HTTPS origin and
  uses its port for the default HTTPS listener. Without an explicit port,
  that default is 443; without SERVER it stays 9743. The older
  `--shared-address SERVER` spelling keeps its 1.0 listener default of
  9743. Both forms accept `--remote-bind` to override the listener, but
  they cannot appear together. `serve install` registers the ordinary
  server to start before login under the owning account on Linux or macOS.
  Windows reports boot installation as unsupported. Lifecycle commands
  target the installed setup selected by `--dir`. Plain `serve status`
  stays global and live-only; `--all` includes installed stopped or
  failed setups.
- **Reconfiguration:** `serve install` without new values keeps the
  saved settings, installs the current executable, and starts the
  service. On first install, the positional URL sets the listener port. Later URL changes keep the saved listener unless the caller
  supplies `--remote-bind`. New options replace saved values after
  validation. `stop` keeps boot startup; `uninstall` removes startup
  and stops the service while preserving pools, keys, and certificates.
- **Input:** `feed` accepts exactly one source: inline JSON, a file, or stdin.
  `--file -` means stdin. Keep `--in` and `--errors` for stream parsing.
  `duplex` reads JSON from piped stdin; terminal input requires `--me` and
  creates `{"from": NAME, "msg": LINE}` messages. `access connect` reads a key
  from a hidden prompt or stdin. Background commands never prompt for repair.
- **History:** `follow` starts with new messages. `--tail N` or `--since TIME`
  adds retained history. `--no-follow` reads the selected history and exits;
  it requires `--tail` or `--since`. Fix the history's upper sequence bound at
  command start so concurrent writes cannot keep a finite read running.
  Resolve relative `--since` times at that same point. If retention overtakes
  the read, report dropped messages and continue through remaining history.
  `--tail` counts retained messages before applying filters; history and live
  output use the same filters. Local and remote reads share these rules.
- **Playback:** `--replay SPEED` sets local history playback speed: `1` means
  original timing, `0` means immediate output. It requires a history selector
  and implies `--no-follow`; normal finite reads need no playback flag.
- **Filters:** Repeat `--tag` and `--where` for AND. Expressions address the
  message envelope, including `.data`. `--one` exits after the first match;
  `--timeout` measures time without output and exits 124. `--data-only`
  selects the payload instead of the envelope on commands that support it.
- **Access:** Keys grant full access to one served directory. `access list`
  reads local saved destinations without probing servers. `access status`
  performs a read-only check of one server and reports problems; a completed
  check exits zero even when access fails. `disconnect` forgets credentials;
  `revoke` withdraws server permission; `untrust` removes browser certificate
  trust. Each operation has its own scope.
- **Errors:** Errors go to stderr with a cause and an actionable next step;
  `--json` selects a structured error envelope, and the default is readable text.
  Preserve the documented error kinds and command-specific exit meanings.
  Command help explains whether a nonzero exit means failure, rejected input,
  corruption, a timeout, or the wrapped process's result.

## Help and release contract

Root help presents the pool model, a create/follow/feed example, the command
signatures in the order above, global options, and links to the CLI guide and
cookbook. Command help owns syntax, input/output, target scope, and constraints.
Both `-h` and `--help` contain essential facts. The CLI guide owns shared rules;
the cookbook owns recipes. `pls` uses the same interface as `plasmite`.

Plasmite 1.0 shipped the shared output, target, and history rules.
Plasmite 1.1 adds positional `serve SERVER` and `access invite NAME`, with the prior option spellings still accepted. The
positional server URL has a new listener-port default; the older
`--shared-address` form keeps its 1.0 meaning. The two forms are not
exact aliases for listener selection.
