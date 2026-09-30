# CLI operating model

This guide explains the behavior shared across Plasmite commands. Use
`plasmite --help` to choose a command, `plasmite <command> --help` for its
exact arguments, and the [cookbook](cookbook.md) for complete workflows.

## Pool references

Local commands accept a pool name such as `events` or an explicit
`.plasmite` path. Names resolve beneath the pool directory, which defaults to
`~/.plasmite/pools`. Put top-level options before the command:

```console
plasmite --dir ./pools follow events
```

Commands with remote support accept `https://host:port/pool`. This shorthand
names a pool; do not append remote API paths. The loopback-only HTTP listener
is for credential-free use on the server machine.

| Command | Local name/path | Remote URL |
| --- | --- | --- |
| `pool create/info/list/delete` | yes | no |
| `feed` | yes | yes |
| `fetch` | yes | no |
| `follow` | yes | yes |
| `tap` | yes | no |
| `duplex` | yes | yes |
| `doctor` | yes | no |

An access key grants full access to every pool in the server’s pool directory:
it can list, read, and append messages, and create or delete pools. For native
remote access, the owner creates a key with `access invite`; the recipient runs
`access connect SERVER_URL` and enters the key at the hidden prompt. The client
saves the connection for the current OS user. Remote `feed`, `follow`, and
`duplex` use saved credentials selected by destination. Use
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
  JSON-RPC to stdout. Use `mcp --dir` for local pools or
  `mcp --remote SERVER_URL` for a saved native HTTPS connection.

## Output

Use human output for inspection and machine output for scripts:

- Pool management and `doctor` expose `--json`.
- `fetch` always emits a JSON message envelope.
- `feed` receipts and `version` adapt to stdout: human text on
  a terminal and JSON when piped.
- `follow` and `duplex` use readable terminal output by default. Select JSON
  Lines with `--format jsonl` or `--jsonl`.
- `mcp` reserves stdout for JSON-RPC.

Message envelopes contain `seq`, `time`, `meta`, and `data`. Compatibility
guarantees for machine output live in the [CLI specification](../spec/v0/SPEC.md).

## Errors and exits

Top-level `--dir` and `--color` must precede the command. `mcp --dir` is a
command-scoped override; when both forms are present, the MCP value wins.

Errors go to stderr. They are concise text on a terminal and a JSON error
envelope when stderr is piped. Argument parsing and help output remain plain
text. General exit codes are mapped by error kind; these command workflows
also have specific meanings:

- `follow` and `duplex` return 124 on timeout.
- `feed --errors skip` returns 1 if any input record was rejected.
- `tap` returns the child process's status, including a signal-derived status.
- `pool delete` is nonzero if any requested deletion fails.
- `doctor` is nonzero if any inspected pool is corrupt.

For examples, see the [cookbook](cookbook.md). For stable scripting contracts,
see the [CLI specification](../spec/v0/SPEC.md).
