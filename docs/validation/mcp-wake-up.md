# MCP resource notifications and agent wake-up

November 20, 2026 · Ergo `IVQ245`, `ELZ7QS`, and `KJH6JX` in [the MCP modernization plan](https://github.com/sandover/plasmite/commit/6ea3d83978ee430cfe3af76e2cb11df53b8a3b9d). These are disposable Linux checks with synthetic messages, not hosted-client acceptance tests.

## What passed

- **MCP Python SDK 2.3.0, Streamable HTTP, protocol 2026-07-28:** the SDK subscribed to precisely `plasmite:///pools/project`. On the wire, the listener acknowledged `subscriptions/listen` with `params.notifications.resourceSubscriptions` containing that URI. An update to a separate pool was filtered out. The matching update delivered `notifications/resources/updated`, and the client read sequence 1. After the stream disconnected, a new read found sequence 2; it was not replayed on the new subscription. This was a standalone resource fixture, not Plasmite serving subscriptions and not an agent turn.
- **Plasmite 2.0.0-dev, base `bad7739` with local changes:** a local CLI follower filtered out sequence 1 with the wrong tag and delivered sequence 2 with the matching tag. After stopping that follower, sequence 3 had the wrong tag and sequence 4 matched. Restarting from `--since 3` delivered sequence 4 without repeating sequence 2. This verifies local filtering and catch-up, not Claude Code Monitor or an agent reply.

Run the standalone SDK fixture in two terminals (Python 3.12 was tested):

```sh
cd docs/validation/mcp-wake-up
IVQ245_PORT=38045 uv run --python 3.12 --with mcp==2.3.0 python server.py
# In the second terminal, start in the same directory:
IVQ245_PORT=38045 uv run --python 3.12 --with mcp==2.3.0 python client.py
```

The server binds only to loopback. Restart it before repeating the fixture. The test prints the received resource update, the result of the catch-up read, and whether the new stream replayed an event.

## Host evidence and recommendation

**Codex CLI 0.172.0-alpha.2:** the normal app-server could not open its state database in the read-only home. An isolated scratch home let it initialize, but its ephemeral thread hit the sandbox socket-directory check. `codex exec --ephemeral` also stopped at initialization with a read-only filesystem error. Neither attempt reached the MCP fixture. [Exact-version source](https://github.com/openai/codex/blob/rust-v0.172.0-alpha.2/codex-rs/rmcp-client/src/tool_list_changes.rs) shows a tool-list tracker; it does not prove an agent wake-up.

**Claude Code:** no authenticated client was available for either the Haiku Monitor or resource-subscription test. [Claude Code’s MCP docs](https://code.claude.com/docs/en/mcp#mcp-client-runtimes) distinguish list refresh from the separate legacy-only Channels path; neither establishes that a standard resource update starts a turn. [Monitor](https://code.claude.com/docs/en/tools-reference#monitor-tool) is another path to test, not one verified here.

Use a tagged follow and a saved next sequence for an agent that is already running. For an idle agent, first test the shortest host-native path: two signed-in Haiku sessions, one Monitor watching a tag, a decoy message, then a matching message. Require the receiver to start a turn and reply without human input, and then test restart catch-up. Do not add standard Plasmite subscriptions solely for wake-up until a target host demonstrates a turn or a bridge is explicitly desired. The SDK fixture proves notification delivery to custom code; that code still needs an authorized way to start a turn and a durable sequence cursor.
