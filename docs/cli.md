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

`feed`, `fetch`, `follow`, and `duplex` accept `udp://HOST:PORT/POOL`. Give an explicit port and one pool name. UDP feed confirms local socket admission. UDP reads deliver datagrams in arrival order, including late messages and duplicates. A fetch ends at terminal progress and can leave messages in flight. Only an explicit server announcement reports loss. The maximum datagram size is 65,507 bytes, including routing and framing. [UDP receive policy](../spec/udp/v1/SPEC.md#receive-ordering-and-notices)

| Command | Local name/path | HTTP(S) URL | UDP URL |
| --- | --- | --- | --- |
| `pool create/delete` | yes | no | no |
| `pool info` | yes | yes | no |
| `pool list [SERVER]` | yes | yes (server origin) | no |
| `feed` | yes | yes | yes |
| `fetch` | yes | yes | yes |
| `follow` | yes | yes | yes |
| `duplex` | yes | yes | yes |
| `doctor` | yes | no | no |

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
`--udp-follow` enables both input and follow at that address. Existing `--udp`
settings keep input only until you opt into reads. Both capabilities apply to
all pools in the selected directory. A reachable sender can append without
credentials; with follow enabled it can also read those pools. Bind only on a
network whose rules limit access. See
[UDP messages](record/serving.md#send-udp-messages) for the access and delivery
contract.

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
listener and follow capability; leaving UDP options off an update keeps the
saved settings. `serve install --udp-follow` enables reads;
`serve install --no-udp-follow` removes reads and keeps input.

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
plasmite fetch source --last 100 > messages.jsonl
plasmite feed destination < messages.jsonl
```

`feed` writes the complete newly appended message to stdout. This makes
`feed first | feed second` a copying chain. Errors and notices go to stderr.
A successful append remains stored if stdout later fails; Plasmite stops
without retrying that append.

## Creating messages

Ordinary JSON or YAML objects are application data; no flag is needed:

```bash
plasmite feed events '{"temperature":21}'
plasmite feed events --tag sensor '{"temperature":21}'
jq -c '.[]' readings.json | plasmite feed events
```

Pass a positional argument or redirect input to stdin. A complete message has exactly four fields, `seq`, `time`, `meta`, and `data`, with only `tags` inside `meta`. Source `seq` must be a positive unsigned integer, `time` can be any valid RFC 3339 timestamp, `meta.tags` must list strings, and `data` must be an object. Invalid messages with that exact shape produce `Usage`. Objects with partial envelopes or extra fields count as application data.

`--tag` adds tags to application objects. Complete messages supply their own tags and reject `--tag`. Non-object application data produces `Usage`.

Use `feed --create` to create a missing pool at the default size. To choose
a size, run `pool create --size SIZE` first.

## Formats

Global `--format pretty|json|yaml|lite3|lite3-raw` chooses output before or after a command. With no flag, a terminal gets readable output and a pipe or file gets compact JSON. `--format` controls output only.

| Format | Output and input |
| --- | --- |
| `pretty` | Readable JSON messages; JSON input accepts them |
| `json` | One compact JSON message per line; input also accepts multiline objects |
| `yaml` | One YAML document per message, starting with `---` and ending with `...` |
| `lite3` | A binary stream with a versioned prefix and a length for each message |
| `lite3-raw` | One raw attachment through EOF; fetch output only |

`feed` detects JSON, marked YAML, and framed Lite3 from their syntax as soon as the first message arrives. It never guesses a message's meaning from application keys. `--input-format auto|json|yaml|lines|lite3|lite3-raw` selects an explicit input encoding. Explicit YAML also accepts one unmarked document through EOF. YAML input accepts JSON-compatible values and rejects custom tags, aliases, non-string keys, and non-finite numbers.

Use `--input-format lines` to store each UTF-8 line as `{"line":"..."}`. It keeps blank lines and the last line even if it lacks a newline.

```bash
plasmite follow source --format yaml | plasmite feed destination
plasmite follow source --format lite3 | plasmite feed destination
plasmite fetch source 1 --format lite3-raw > heartbeat.lite3
plasmite feed destination --input-format lite3-raw < heartbeat.lite3
```

Framed Lite3 starts with the four bytes `PLS2`. Each record starts with `M` and a four-byte body length. The body holds an eight-byte sequence, an eight-byte timestamp in nanoseconds and the original Lite3 payload. Integers use big-endian byte order. Raw attachments have no prefix or record header. Copying preserves the original Lite3 payload bytes.

Each Lite3 payload may occupy at most 256 MiB and must fit the destination pool. The text reader permits encoding overhead rather than imposing the old 1 MiB record limit. Remote servers keep their configured request-body limit (1 MiB by default); use `serve --max-body-bytes` when larger messages need remote delivery. Truncated or malformed input stops the command and returns nonzero. Earlier successful appends remain. Empty input streams succeed without appending anything.

Reports use the same JSON/YAML formatting rule but remain reports. Binary formats require message commands; raw output requires `fetch`. Help and completion remain text. `mcp` keeps JSON-RPC. `duplex` accepts the same messages as `feed` on terminal and piped stdin and streams received complete messages, including its own appends. Input EOF closes sending while reception continues. Stop the process or close stdout to end the receive session. Use `feed` for finite copying. See the [2.0 upgrade guide](record/upgrading-2.0.md) for migration.

Bash and Zsh preserve binary pipes. PowerShell 7.4 and later preserves native binary redirection; earlier versions need `cmd.exe` redirection.

## Reading history

`follow POOL` waits for new messages. Use `--last N` to read N retained messages before waiting for new ones. `--since` accepts `oldest`, an inclusive sequence, an RFC 3339 time or a duration such as `5m`. Use `--one` to wait for one matching message and exit in any shell.

`fetch POOL` reads retained history and exits. It takes the same start and tag options. Supply a sequence as a positional argument to fetch one message.

```console
plasmite fetch events
plasmite fetch https://pools.example.net:9743/events --since 5m --format json
plasmite follow events --since 101 --tag gfx --one
```

UDP follows use their separate [arrival-order delivery policy](../spec/udp/v1/SPEC.md#receive-ordering-and-notices).

A fetch stops at the last sequence that existed when the read opened. `--last` counts retained messages before filtering. Repeat `--tag` to require every specified tag. Use `jq` after Plasmite to filter on data fields.

If the pool overwrites a live reader's next message, `follow` reports the lost range on stderr and continues at the first sequence still retained. `fetch` reports a `RetentionGap` and exits nonzero.

## Errors and exits

Errors and notices go to stderr. They follow the selected text format: readable
text in a terminal, compact JSON in a pipe or file, or marked YAML with
`--format yaml`. Binary output uses readable diagnostics. Help stays readable text.
General exit codes are mapped by error kind; these command workflows
also have specific meanings:

- `feed` stops on invalid input; earlier successful appends remain stored.
- `pool delete` is nonzero if any requested deletion fails.
- `doctor` is nonzero if any inspected pool is corrupt.

For examples, see the [cookbook](cookbook.md). For stable scripting contracts,
see the [2.0 CLI contract](../spec/v2/SPEC.md#cli).

### Retrying writes

A local flush or remote connection can fail after the pool stores the message.
Check the pool before sending it again.
