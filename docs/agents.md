# Plasmite for agents

Agents come and go. A pool keeps recent messages on disk, so an agent can read what happened before it started or while it was away. Agents and people share the same pools: you can watch the exchange with `pls follow` and join in with `pls feed`.

## Connect an agent

Plasmite requires a Model Context Protocol (MCP) client that uses revision [`2026-07-28`](https://modelcontextprotocol.io/specification/2026-07-28). Clients that only use the older `initialize` handshake cannot connect.

- [Claude Code](https://code.claude.com/docs/en/mcp#mcp-client-runtimes) needs its v2 MCP runtime with modern protocol negotiation. If it is using the older runtime or handshake, launch a recent version with `MCP_SDK_GENERATION=v2` and `MCP_PROTOCOL_NEGOTIATION=auto`.
- For a [Codex build that supports the modern MCP feature](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#hosted-codex-apps-mcp-protocol), set `mcp_2026_07_28 = true` under `[features]` in `config.toml`. The setting for hosted Codex Apps does not apply to Plasmite.

These are the host settings, not a guarantee that every installed build supports the connection.

**For direct access to local pool files**, install Plasmite where the client runs and configure the client to launch it over standard input and output:

```bash
claude mcp add --transport stdio plasmite -- plasmite mcp
codex mcp add plasmite -- plasmite mcp       # Codex
```

Other clients take the same command in their configuration: `{"mcpServers": {"plasmite": {"command": "plasmite", "args": ["mcp"]}}}`.

**For HTTPS access**, configure the client to use the shared server's `/mcp` endpoint. You do not need to install Plasmite on the client machine. The MCP client and the browser must both trust the server's certificate. The client must support [Client ID Metadata Documents (CIMD)](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization#client-registration) for registration. Plasmite does not offer Dynamic Client Registration (DCR) or a device sign-in flow.

```bash
claude mcp add --scope user --transport http plasmite https://pools.example.net:9743/mcp
codex mcp add plasmite --url https://pools.example.net:9743/mcp --oauth-resource https://pools.example.net:9743/mcp
```

Use the command for your client. If it does not open a browser for sign-in, run `claude mcp login plasmite` or `codex mcp login plasmite`. Enter the Plasmite access key on the server's browser approval page, not in a chat or a header. See [Connect an AI client](record/serving.md#connect-an-ai-client) for setup and [the validation report](validation/mcp-wake-up.md) for what has been tested. The commands above are for builds with the required protocol and registration support; we have not verified end-to-end sign-in in Claude Code or Codex.

## The tools

| Tool | What it does |
|---|---|
| `plasmite_feed` | Add a message: `pool`, `data`, optional `tags`, and `create` to make the pool if it's missing |
| `plasmite_fetch` | Read what the pool holds and return, from `since` (a sequence, `oldest`, a time or a duration) or `last` |
| `plasmite_follow` | During a running turn, wait up to `timeout_ms` (default 10 seconds, at most 60) for new messages |
| `plasmite_pool_list`, `_info`, `_create`, `_delete` | Manage pools |

Reads return up to `count` messages (default 20, at most 200) plus `next_seq`. Pass `next_seq` as `since` on the next call to carry on exactly where the last one stopped. `tags` keeps only messages that carry every listed tag.

## Patterns

### Hand off work

One agent finishes and says so:

```json
{"pool": "project", "tags": ["handoff"], "data": {"from": "claude:api", "status": "done", "branch": "fix-login", "summary": "login tests pass"}}
```

On its next turn, the receiving agent calls `plasmite_fetch` with `{"pool": "project", "tags": ["handoff"], "last": 20}`, or uses its saved sequence to read from where it left off.

An incoming message does not start a turn in an idle agent. Plasmite does not currently serve standard MCP resource subscriptions. An in-flight `plasmite_follow` call is a bounded read in an existing turn, not an idle wake-up. A host refreshing its tools or resources is not the same as the host starting a turn.

### Catch up after a restart

A fresh session can read recent history: `plasmite_fetch` with `{"pool": "project", "last": 20}`. To resume from a known position, save `next_seq` after handling the returned messages and pass it as `since` on the next read.

If the pool overwrote messages the agent hadn't read, the read fails with a `RetentionGap` error that names the first missed sequence and the oldest one still held. The agent can carry on from there or rebuild from another source.

### Keep a person in the loop

Watch your agents in a terminal:

```bash
pls follow project
```

Send them an instruction:

```bash
pls feed project --tag instruction '{"from": "alice", "text": "pause the migration until I review it"}'
```

Agents that check for `instruction` messages between steps will see it.

## Tell your agents how to coordinate

Agents follow their instructions file. Paste something like this into `AGENTS.md` or `CLAUDE.md`, and adjust the names:

```markdown
## Coordination
Use the plasmite MCP tools and the `project` pool to coordinate with other agents.
- Before you start, fetch the last 20 messages to see what others are doing.
- Post progress and results with your name in `data.from`, for example `claude:api`.
- When you finish a task, post it tagged `handoff` with a one-line summary.
- Between steps, check for messages tagged `instruction` and follow them.
- After a restart, use your saved next sequence to read any retained handoffs you have not handled.
```

## Conventions that help

- **Name the sender in `data`.** Plasmite doesn't record who wrote a message, so put it in `data.from`, such as `claude:api` or `codex:ci`.
- **Use tags for topics** such as `handoff`, `instruction` and `error`, so each reader asks only for what it needs.
- **Use one pool per project,** sized for the history your agents should be able to read back: `pls pool create project --size 8M`.
- **Retry a feed with care.** A feed that failed with a connection error may have been stored anyway. Give important messages an `id` in `data` so readers can spot a duplicate.
