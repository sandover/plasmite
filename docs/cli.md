# CLI operating model

This guide explains the behavior shared across Plasmite commands. Use
`plasmite --help` to choose a command, `plasmite <command> --help` for its
exact arguments, and the [cookbook](cookbook.md) for complete workflows.

## Pool references

Local commands accept a pool name such as `events` or an explicit
`.plasmite` path. Names resolve beneath the pool directory, which defaults to
`~/.plasmite/pools`. Global `--dir` and `--color` work before or after a
command. One directory applies to the entire invocation, including `mcp`;
conflicting repeated directory values are rejected:

```console
plasmite --dir ./pools follow events
```

Commands with remote support accept `https://host:port/pool`. This shorthand
names a pool; do not append remote API paths. The loopback-only HTTP listener
is for credential-free use on the server machine.

| Command | Local name/path | Remote URL |
| --- | --- | --- |
| `pool create/delete` | yes | no |
| `pool info` | yes | yes |
| `pool list [SERVER]` | yes | yes (server origin) |
| `feed` | yes | yes |
| `fetch` | yes | yes |
| `follow` | yes | yes |
| `tap` | yes | no |
| `duplex` | yes | yes |
| `doctor` | yes | no |

## Serving and startup

`plasmite --dir ./shared serve https://pools.example.net:9743` runs in the
foreground. The positional URL names the public HTTPS origin and sets the
default remote listener port. Without a URL, the HTTPS listener remains at
`0.0.0.0:9743`. A URL without an explicit port uses HTTPS port 443.
`--remote-bind` overrides the listener when a proxy uses a different
backend port or a specific interface. The URL hostname never selects the
bind interface. The local administration listener stays on loopback.

For small messages whose senders accept possible loss, `--udp` adds an
append-only UDP listener alongside HTTP and HTTPS. It binds to
`127.0.0.1:9701` by default; use `--udp=IP:PORT` to choose another address.
The listener can append to every pool in the selected directory. A non-loopback
address grants unauthenticated append access to that directory to reachable
senders, so bind it only on a network whose rules limit access. See
[UDP input](record/serving.md#send-udp-messages) for its message format and
delivery limits.

The older `--shared-address SERVER` form still names the public origin but
keeps the 1.0 listener default of `0.0.0.0:9743`. Use `--remote-bind` to
change its listener. Supply the public origin through one form only.

The command below installs startup before login and starts the server for
the selected pool:

```console
plasmite --dir ./shared serve install https://pools.example.net:9743
```

On Linux and macOS, the service runs under the account that owns the pool.
On Windows, Plasmite registers an automatic Windows service under a
pool-specific virtual account. Install, update, and uninstall need approval
through User Account Control (UAC) from the administrator who owns the pool. Plasmite stores
no login password. The executable and service setup live under
`Program Files\Plasmite\Services`; shared service state and retained logs
live in `.plasmite-serve` inside the pool directory. Saved client credentials
remain private to the account that saved them.

The service can list parent-folder names along its pool path so Windows can
prevent those folders from moving during private-state access.

The service can start before an interactive sign-in when Windows can access
the pool and its storage at boot. If the server uses Tailscale, enable
Tailscale unattended mode for network access before sign-in. An encrypted or
removable volume that unlocks or mounts only after sign-in delays the server.
Pool paths may contain spaces or Unicode. Use an absolute `--dir` path for
lifecycle commands run from another working directory:

```console
plasmite --dir ./shared serve start
plasmite --dir ./shared serve stop
plasmite --dir ./shared serve restart
plasmite --dir ./shared serve logs
plasmite --dir ./shared serve uninstall
```

`stop` leaves startup enabled. `uninstall` stops the service and removes
startup without deleting pools, access keys, or certificates. On Windows,
updates stop the service before replacing its executable and setup; a failed
update restores the prior service while preserving its identity, certificates,
and keys. These commands target the installed setup; they do not control a
foreground server. Run
`serve install` with new options to update a saved setup. The positional
URL chooses the default listener port on first install. Later URL changes
keep the saved listener unless you supply `--remote-bind`. With no new
options, `install` keeps the saved settings, installs the current Plasmite
executable, and starts the service. Updates retain the installed listener
addresses unless `--remote-bind` changes them. UDP settings also persist across
updates and restarts. Use `serve install --no-udp` to remove the saved UDP
listener; leaving `--udp` off an update keeps its current setting.

## Server status

`plasmite serve status` shows Plasmite servers running for your current OS
user, including the pool directory and local and remote addresses. It finds
servers started with different `--dir` values, so the top-level `--dir` option
does not narrow this list. If no server is running, it prints
`No Plasmite servers running.`

Only servers started with this version register. Restart older running servers
to make them appear. Stopped servers do not appear in the list.

```console
plasmite serve status
plasmite serve status --format json
```

The JSON form is an array. Each object has `pid`, `pool_dir` (an absolute
path), `local_url`, and `remote_url`. `remote_url` uses the positional server
URL, `--shared-address`, or a loopback HTTPS bind. It is `null` when the
client-facing HTTPS address is unknown. Secure serving still has a remote
listener.

`serve status --all` also lists installed setups that stopped or failed.
It remains global: `--dir` does not narrow it. Its JSON rows have
`pool_dir`, nullable `pid`, `local_url`, nullable `remote_url`,
`managed`, `startup`, `state`, nullable `problem`, and nullable `setup`.
States include `running`, `starting`, `stopped`, and `failed`.
The setup object holds the saved directory, executable, account, home, and
serve options. An installed setup's addresses may describe saved settings
rather than live listeners.

## Saved connections

`access list` lists this user's saved server destinations without contacting
them. Use `access list --format json` for an array of objects with a `destination`
field. `access status SERVER_URL` checks one destination's reachability and
authorization. It prints the report and exits nonzero when the server is
unreachable or rejects access.

An access key grants full access to every pool in the server’s pool directory:
it can list, read, and append messages, and create or delete pools. For native
remote access, the owner creates a key with `access invite NAME`; the recipient runs
`access connect SERVER_URL` and enters the key at the hidden prompt. The client
saves the connection for the current OS user. Remote `feed`, `follow`, and
`duplex`, remote `fetch`, and remote pool inspection use saved credentials
selected by destination. Use
`access status SERVER_URL` to check the connection and `access disconnect SERVER_URL` to
forget it locally, even while the server is offline. Disconnect does not
revoke the server key. See [Share your first pool](record/serving.md#share-your-first-pool)
for setup, and the [serving guide](record/serving.md) for access recovery and
browser or MCP connections.

For shared pools, configure the harness to connect directly to the server's
HTTPS `/mcp` address. The harness completes OAuth authorization in the browser;
the local Plasmite CLI and its saved native connections are not involved. Local
and direct HTTPS MCP follow the 2025-11-25 handshake and share the same tools
and resources. See the [serving guide](record/serving.md#connect-an-ai-client)
for setup and the [MCP contract](../spec/mcp/2025-11-25/SPEC.md) for message
and HTTP details.

## Messages and pipes

`feed`, `fetch`, `follow`, and `duplex` use complete messages. A JSON message
contains an object `data` and `meta.tags`, a list of strings. Stored messages
also contain `seq` and `time`. When `feed` copies a message, it preserves the
contents and tags and assigns a new destination sequence and time.

```bash
# Copy new messages, including their tags.
plasmite follow source | plasmite feed destination

# Change one field and preserve the rest.
plasmite follow source |
  jq -c '.data.temperature += 1' |
  plasmite feed destination

# Save retained messages and load them later.
plasmite follow source --tail 100 --no-follow > messages.jsonl
plasmite feed destination < messages.jsonl
```

`feed` writes the complete newly appended message to stdout. This makes
`feed first | feed second` a copying chain. Errors and notices go to stderr.
A successful append remains stored if stdout later fails; Plasmite stops
without retrying that append.

## Creating messages

Use `--data` to create messages from application objects. This gives ordinary
fields such as `data`, `seq`, and `meta` their application meaning.

```bash
plasmite feed events --data '{"temperature":21}'
plasmite feed events --data --tag sensor '{"temperature":21}'
jq -c '.[]' readings.json | plasmite feed events --data
```

Choose one source: a positional argument, `--file PATH`, or stdin.
`--file -` reads stdin. All sources follow the same message rule. `--tag`
requires `--data`; copied messages supply their own tags. Missing `meta` or
`meta.tags` means an empty tag list. Default message input rejects unknown
message and metadata fields, invalid tags, and non-object contents.

## Formats

Global `--format pretty|json|yaml|lite3|lite3-raw` chooses output before or
after a command. With no flag, a terminal gets readable output and a pipe
or file gets compact JSON. `--format` controls output only.

| Format | Output and input |
| --- | --- |
| `pretty` | Readable JSON messages; JSON input accepts them |
| `json` | One compact JSON message per line; input also accepts multiline objects |
| `yaml` | One YAML document per message, starting with `---` and ending with `...` |
| `lite3` | A binary stream with a versioned prefix and a length for each message |
| `lite3-raw` | One raw attachment through EOF; fetch output only |

`feed` detects JSON, marked YAML, and framed Lite3 from their syntax as soon
as the first message arrives. It never guesses a message's meaning from
application keys. `--input-format auto|json|yaml|lite3|lite3-raw` selects an
explicit input encoding. Explicit YAML also accepts one unmarked document
through EOF. YAML input accepts JSON-compatible values and rejects custom
tags, aliases, non-string keys, and non-finite numbers.

```bash
plasmite follow source --format yaml | plasmite feed destination
plasmite follow source --format lite3 | plasmite feed destination
plasmite fetch source 1 --format lite3-raw > heartbeat.lite3
plasmite feed destination --input-format lite3-raw < heartbeat.lite3
```

Framed Lite3 starts with the four bytes `PLS1`. Each record then contains
an eight-byte source sequence, an eight-byte timestamp in nanoseconds,
a four-byte payload length, and that many Lite3 bytes. Header integers use
big-endian byte order, matching the existing HTTP binary stream records.
The prefix belongs to the CLI stream. Raw attachments have no prefix or
record header. Copying preserves the original Lite3 payload bytes.

Each Lite3 payload may occupy at most 256 MiB and must fit the destination
pool. The text reader permits encoding overhead rather than imposing the
old 1 MiB record limit. Remote servers keep their configured request-body
limit (1 MiB by default); use `serve --max-body-bytes` when larger messages
need remote delivery. Truncated or malformed input stops the command and
returns nonzero. Earlier successful appends remain. Empty input streams
succeed without appending anything.

Reports use the same JSON/YAML formatting rule but remain reports.
Binary formats require message commands; raw output requires `fetch`.
Help and completion remain text. `tap` passes through its child's output;
`mcp` keeps JSON-RPC. Terminal `duplex` creates chat messages with `--me`;
piped `duplex` accepts complete messages, streams received messages, and
ends when stdin closes. Use `feed` when every input needs an echoed append.

Bash and Zsh preserve binary pipes. PowerShell 7.4 and later preserves
native binary redirection; earlier versions need `cmd.exe` redirection.

## History and playback

`follow POOL` waits for new messages. `--tail N` adds the last N retained
messages, and `--since TIME` adds messages since an RFC 3339 or relative time
such as `5m`. Local and remote reads follow these same rules.

Use `--no-follow` with either selector to read history and exit:

```console
plasmite follow events --tail 100 --no-follow
plasmite follow https://pools.example.net:9743/events --since 5m --no-follow --format json
```

A finite read fixes its upper sequence boundary when it starts. Later writes
cannot keep it running. Relative times use that same starting point. `--tail`
counts retained messages before filters; `--tag` and `--where` then select
matches within that history and use the same filters for new messages.
If retention overtakes a live reader, it reports dropped messages and
continues. A finite history read reports the loss and exits nonzero. `--one` exits at the first
match; a timeout exits 124.

`--replay SPEED` controls local history timing and implies a finite read.
It requires `--tail` or `--since`. Speed `1` preserves timing, `2` doubles it,
and `0` emits immediately. Remote replay timing remains unsupported.

## Errors and exits

Errors and notices go to stderr. They follow the selected text format: readable
text in a terminal, compact JSON in a pipe or file, or marked YAML with
`--format yaml`. Binary output uses readable diagnostics. Help stays readable text.
General exit codes are mapped by error kind; these command workflows
also have specific meanings:

- `follow` and `duplex` return 124 on timeout.
- `feed` stops on invalid input; earlier successful appends remain stored.
- `tap` returns the child process's status, including a signal-derived status.
- `pool delete` is nonzero if any requested deletion fails.
- `doctor` is nonzero if any inspected pool is corrupt.

For examples, see the [cookbook](cookbook.md). For stable scripting contracts,
see the [CLI specification](../spec/v0/SPEC.md).

### Retrying writes

With `feed --retry`, local and remote appends retry `Busy` errors.
An I/O failure ends the command: a local flush or remote connection can fail
while the message already exists in the pool. Check the pool before sending
it again.
