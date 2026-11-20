# Plasmite for agents

Agents come and go. Sessions end, contexts get compacted, harnesses restart. A pool outlasts all of that: an agent that starts fresh can read what happened while it was away, then wait for what comes next. Agents and people share the same pools, so you can watch your agents talk with `pls follow`, and join in.

Claude Code and Codex reach pools through Plasmite's Model Context Protocol (MCP) server.

## Connect an agent

**For direct access to local pool files**, configure the client to launch `plasmite mcp` and talk to it over stdin and stdout:

```bash
claude mcp add plasmite -- plasmite mcp      # Claude Code
codex mcp add plasmite -- plasmite mcp       # Codex
```

Other clients take the same command in their configuration: `{"mcpServers": {"plasmite": {"command": "plasmite", "args": ["mcp"]}}}`.

**For HTTPS access**, `plasmite serve` exposes `/mcp` automatically. Clients on the same machine or another machine can connect there without a separate Plasmite process. Sign in with an access key in the browser:

```bash
claude mcp add --transport http plasmite https://pools.example.net:9743/mcp
```

The server needs a certificate the agent's machine trusts. [Connect an AI client](record/serving.md#connect-an-ai-client) covers the certificate, Codex and sign-in.

## The tools

| Tool | What it does |
|---|---|
| `plasmite_feed` | Add a message: `pool`, `data`, optional `tags`, and `create` to make the pool if it's missing |
| `plasmite_fetch` | Read what the pool holds and return, from `since` (a sequence, `oldest`, a time or a duration) or `last` |
| `plasmite_follow` | Wait up to `timeout_ms` (default 10 seconds, at most 60) for new messages |
| `plasmite_pool_list`, `_info`, `_create`, `_delete` | Manage pools |

Reads return up to `count` messages (default 20, at most 200) plus `next_seq`. Pass `next_seq` as `since` on the next call to carry on exactly where the last one stopped. `tags` keeps only messages that carry every listed tag.

## Patterns

### Hand off work

One agent finishes and says so:

```json
{"pool": "project", "tags": ["handoff"], "data": {"from": "claude:api", "status": "done", "branch": "fix-login", "summary": "login tests pass"}}
```

Another agent waits for it by calling `plasmite_follow` with `{"pool": "project", "tags": ["handoff"]}`, and calls again with `since` set to `next_seq` until a handoff arrives. A script can wait the same way: `pls follow project --tag handoff --one && ./deploy.sh`.

### Catch up after a restart

A fresh session starts by reading recent history: `plasmite_fetch` with `{"pool": "project", "last": 20}`. An agent that saved `next_seq` from its last read passes it as `since` and misses nothing.

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
- To wait for another agent, follow with tags ["handoff"] and call again with `since` set to `next_seq`.
```

## Conventions that help

- **Name the sender in `data`.** Plasmite doesn't record who wrote a message, so put it in `data.from`, such as `claude:api` or `codex:ci`.
- **Use tags for topics** such as `handoff`, `instruction` and `error`, so each reader asks only for what it needs.
- **Use one pool per project,** sized for the history your agents should be able to read back: `pls pool create project --size 8M`.
- **Retry a feed with care.** A feed that failed with a connection error may have been stored anyway. Give important messages an `id` in `data` so readers can spot a duplicate.
