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
path), `local_url`, and `remote_url`. `remote_url` uses `--shared-address` or
a loopback HTTPS bind. It is `null` when the client-facing HTTPS address is
unknown; a non-loopback bind needs `--shared-address`, even with a TLS
certificate configured. Secure serving still has a remote listener.

## Saved connections

`access list` lists this user's saved server destinations without contacting
them. Use `access list --json` for an array of objects with a `destination`
field. `access status SERVER_URL` checks one destination's reachability and
authorization. It exits zero when the check completes, even when the report
says access failed; scripts should inspect the JSON fields.

An access key grants full access to every pool in the server’s pool directory:
it can list, read, and append messages, and create or delete pools. For native
remote access, the owner creates a key with `access invite`; the recipient runs
`access connect SERVER_URL` and enters the key at the hidden prompt. The client
saves the connection for the current OS user. Remote `feed`, `follow`, and
`duplex`, remote `fetch`, and remote pool inspection use saved credentials
selected by destination. Use
`access status SERVER_URL` to check the connection and `access disconnect SERVER_URL` to
forget it locally, even while the server is offline. Disconnect does not
revoke the server key. See [Share your first pool](record/serving.md#share-your-first-pool)
for setup, and the [serving guide](record/serving.md) for access recovery and
browser or MCP connections.

After `access connect`, Plasmite prints setup commands for Claude Code and
Codex CLI. Each command starts `plasmite mcp --remote SERVER_URL` over stdio.
The MCP process reads the saved connection before each remote request, so a
later disconnect, replacement, or server-side revocation takes effect on the
next tool call. Local and remote MCP follow the 2025-11-25 handshake and share
the same tools and resources. See the [MCP contract](../spec/mcp/2025-11-25/SPEC.md)
for the message and HTTP details.

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
  JSON-RPC to stdout. Use `--dir DIR mcp` (or `mcp --dir DIR`) for local pools or
  `mcp --remote SERVER_URL` for a saved native HTTPS connection.

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
