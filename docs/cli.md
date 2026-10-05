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

The older `--shared-address SERVER` form still names the public origin but
keeps the 1.0 listener default of `0.0.0.0:9743`. Use `--remote-bind` to
change its listener. Supply the public origin through one form only.

On Linux or macOS, the command below installs startup before login and
starts the server under the account that owns its pools and keys:

```console
plasmite --dir ./shared serve install https://pools.example.net:9743
```

The command may request administrator approval to register the native
service. Windows supports foreground serving and reports that boot
installation is unsupported. Boot startup requires the home, service files,
and pool directory to be available before login. An encrypted or removable
volume that unlocks or mounts only after login delays the server. Use an
absolute `--dir` path for lifecycle commands run from another working
directory:

```console
plasmite --dir ./shared serve start
plasmite --dir ./shared serve stop
plasmite --dir ./shared serve restart
plasmite --dir ./shared serve logs
plasmite --dir ./shared serve uninstall
```

`stop` leaves startup enabled. `uninstall` stops the service and removes
startup without deleting pools, access keys, or certificates. These commands
target the installed setup; they do not control a foreground server. Run
`serve install` with new options to update a saved setup. The positional
URL chooses the default listener port on first install. Later URL changes
keep the saved listener unless you supply `--remote-bind`. With no new
options, `install` keeps the saved settings, installs the current Plasmite
executable, and starts the service.

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
plasmite serve status --json
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
them. Use `access list --json` for an array of objects with a `destination`
field. `access status SERVER_URL` checks one destination's reachability and
authorization. It exits zero when the check completes, even when the report
says access failed; scripts should inspect the JSON fields.

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

## Input

- `feed` accepts one inline JSON value, a file with `--file`, `--file -` for
  stdin, or piped stdin when neither inline data nor a file is given.
  `--in` selects JSON, JSON Lines, or auto-detection for streamed input;
  `--errors` selects stop or skip behavior.
- `tap POOL -- COMMAND...` runs a required child command, passes through its
  stdin, and records its stdout and stderr.
- `duplex` reads line-oriented chat from a terminal (requiring `--me`) and a
  JSON stream from non-terminal stdin.
- `mcp` reads newline-delimited JSON-RPC from stdin until EOF and writes
  JSON-RPC to stdout. Use `--dir DIR mcp` (or `mcp --dir DIR`) to expose local
  pools over stdio. For shared pools, connect the harness directly to the
  server's HTTPS `/mcp` address.

## Output

Commands produce readable output by default, including when piped. Select
`--json` explicitly for scripts. Terminal detection controls presentation,
such as color; it does not select the output contract.

| Commands | `--json` output |
| --- | --- |
| `pool`, `doctor`, `serve status`, `access`, `version` | One JSON document |
| `fetch` | One JSON message envelope |
| `follow`, `duplex` | One JSON message envelope per line |
| `feed` | One append receipt per line |

Structured output has no color or commentary. `--jsonl` and `--format jsonl`
remain aliases for message streams. `mcp` always uses JSON-RPC;
`completion` always prints shell code. `tap` passes through child output;
`--quiet` suppresses that output.

Message envelopes contain `seq`, `time`, `meta`, and `data`. Compatibility
guarantees for machine output live in the [CLI specification](../spec/v0/SPEC.md).

## History and playback

`follow POOL` waits for new messages. `--tail N` adds the last N retained
messages, and `--since TIME` adds messages since an RFC 3339 or relative time
such as `5m`. Local and remote reads follow these same rules.

Use `--no-follow` with either selector to read history and exit:

```console
plasmite follow events --tail 100 --no-follow
plasmite follow https://pools.example.net:9743/events --since 5m --no-follow --json
```

A finite read fixes its upper sequence boundary when it starts. Later writes
cannot keep it running. Relative times use that same starting point. `--tail`
counts retained messages before filters; `--tag` and `--where` then select
matches within that history and use the same filters for new messages.
If retention overtakes the reader, it reports the dropped messages and
continues through the history still available. `--one` exits at the first
match; a timeout exits 124.

`--replay SPEED` controls local history timing and implies a finite read.
It requires `--tail` or `--since`. Speed `1` preserves timing, `2` doubles it,
and `0` emits immediately. Remote replay timing remains unsupported.

## Errors and exits

Errors go to stderr as concise text by default; `--json` selects a structured
error envelope. Help stays readable text. General exit codes are mapped by
error kind; these command workflows
also have specific meanings:

- `follow` and `duplex` return 124 on timeout.
- `feed --errors skip` returns 1 if any input record was rejected.
- `tap` returns the child process's status, including a signal-derived status.
- `pool delete` is nonzero if any requested deletion fails.
- `doctor` is nonzero if any inspected pool is corrupt.

For examples, see the [cookbook](cookbook.md). For stable scripting contracts,
see the [CLI specification](../spec/v0/SPEC.md).

### Retrying remote writes

With `feed --retry`, a remote append retries an explicit `Busy` response.
It returns an I/O failure without retrying: the server may have saved the
message before the connection failed. Check the pool before sending it again.
