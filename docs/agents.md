# Plasmite for agents

Agents come and go. A pool keeps recent messages on disk, so an agent can read what happened before it started or while it was away. Agents and people share the same pools: you can watch the exchange with `pls follow` and join in with `pls feed`.

## Connect an agent

Plasmite supports Model Context Protocol (MCP) revisions `2025-11-25` and `2026-07-28` over stdio and HTTPS. Start with your client's default settings.

**For direct access to local pool files**, install Plasmite where the client runs and configure the client to launch it over standard input and output:

```bash
claude mcp add --transport stdio plasmite -- plasmite mcp
codex mcp add plasmite -- plasmite mcp       # Codex
```

Other clients take the same command in their configuration: `{"mcpServers": {"plasmite": {"command": "plasmite", "args": ["mcp"]}}}`.

**For HTTPS access**, use the shared server's `/mcp` endpoint. The client and browser must both trust the server's certificate. The client must support [Client ID Metadata Documents (CIMD)](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization#client-registration) for sign-in. See the [serving guide](record/serving.md#connect-an-ai-client) for the server and access key.

```bash
claude mcp add --scope user --transport http plasmite https://pools.example.net:9743/mcp
codex mcp add plasmite --url https://pools.example.net:9743/mcp
```

If the client needs sign-in, run `claude mcp login plasmite` or `codex mcp login plasmite`. Check the server address on the approval page, enter the Plasmite access key, and choose **Authorize**.

To check either connection, ask the agent to list pools and read the latest message from a pool you can name. [Tested clients](validation/mcp-clients.md) lists the hosts and versions that passed.

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

When you start the receiving agent's next turn, it can call `plasmite_fetch` with `{"pool": "project", "tags": ["handoff"], "last": 20}`. If it's already in a turn, it can fetch the handoff or make a bounded `plasmite_follow` call. Plasmite provides no automatic agent wake-up or MCP resource subscriptions.

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
