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

## Host evidence and decision

**Codex CLI 0.172.0-alpha.2:** the normal app-server could not open its state database in the read-only home. An isolated scratch home let it initialize, but its ephemeral thread hit the sandbox socket-directory check. `codex exec --ephemeral` also stopped at initialization with a read-only filesystem error. Neither attempt reached the MCP fixture. [Exact-version source](https://github.com/openai/codex/blob/rust-v0.172.0-alpha.2/codex-rs/rmcp-client/src/tool_list_changes.rs) shows a tool-list tracker; it does not prove an agent wake-up.

**Claude Code:** no authenticated client was available for either the Haiku Monitor or resource-subscription test. [Claude Code’s MCP docs](https://code.claude.com/docs/en/mcp#mcp-client-runtimes) distinguish list refresh from the separate legacy-only Channels path; neither establishes that a standard resource update starts a turn.

## MCP Events beta

The earlier experiments checked core resource notifications. The [MCP Events extension](https://modelcontextprotocol.io/extensions/events/overview) adds filtered subscriptions and resumable cursors. OpenCode 2.3.2 started a new turn through its native automation runtime when Plasmite sent a matching `pool.message` event. The turn fetched the message by its sequence number. An append with a nonmatching tag left the agent idle. The test used an isolated local server and a scripted model. See the [OpenCode setup](../agents.md#start-a-turn-from-a-pool-event-in-opencode).

Codex CLI 0.171.0-alpha.13 rejected the app-server event-stream request for a custom server because that path only supports hosted apps. ChatGPT Work documents webhooks from the earlier Events draft. Its guide uses `capabilities.events`, while the beta extension uses `capabilities.extensions`. Check Work against the beta wire format before claiming compatibility. [Codex implementation](https://github.com/openai/codex/blob/rust-v0.171.0-alpha.13/codex-rs/app-server/src/request_processors/mcp_event_stream.rs) · [ChatGPT Work webhook guide](https://developers.openai.com/plugins/build/mcp-events)

Claude Code 2.1.329 connected to a disposable Plasmite HTTP server with its default protocol settings and fetched a known message. The request trace showed modern MCP discovery and tool calls. Claude advertised no client extensions and sent no Events requests during the check. [Claude Code 2.1.329 release](https://github.com/anthropics/claude-code/releases/tag/v2.1.329)


## HTTPS connections

On November 20, 2026, a disposable Plasmite HTTPS server on macOS ARM64 recorded
which HTTP version clients used for ordinary MCP requests. Each client trusted
the fixture's certificate and used a fixture bearer.

| Client | Ordinary MCP requests | Events |
| --- | --- | --- |
| OpenCode 2.3.2 | HTTP/1.1 | An automation held one connection for its stream and opened a second to fetch the matching message. |
| Claude Code 2.1.329 | HTTP/1.1 | The client sent no Events requests in the wake-up check. |
| Codex CLI 0.171.0-alpha.13 | HTTP/2 | The app-server rejected an Events stream for this custom server. |

A scripted local model created the OpenCode automation and fetched the matching
message at sequence 2. The append at sequence 1 used a different tag and left
the automation idle. The server logged HTTP/1.1 throughout. It held one socket
while the automation waited and two after the fetch, with the stream still open.

To isolate the listener's connection cost, a test client made a `GET /healthz`
request on each of a set of separate HTTP/1.1 connections, then left the
connections open. At each count it attempted another 16 health requests on
fresh connections. Both builds used the source at `234cc5b`, with only the
connection cap changed for the second build. The server used release builds on
macOS 27.2.1 with a process limit of 4,096 file descriptors. The test read
resident memory from `ps` and counted open descriptors with `lsof`.

| Listener cap | Open connections | Server memory | Server descriptors | Fresh requests served |
| ---: | ---: | ---: | ---: | ---: |
| 128, baseline at commit `234cc5b` | 128 | 13.7 MiB | 140 | 0 of 16 |
| 2,048 | 0 | 7.2 MiB | 12 | 16 of 16 |
| 2,048 | 1,000 | 44.8 MiB | 1,012 | 16 of 16 |
| 2,048 | 2,000 | 80.2 MiB | 2,012 | 16 of 16 |
| 2,048 | 2,048 | 82.0 MiB | 2,060 | 0 of 16 |

Each idle connection cost one server descriptor and about 38 KiB of resident
memory at 1,000 connections. A separate check opened 64 Events streams and
successfully made a `tools/list` request over one shared HTTP/2 connection.
The idle socket figures measure the listener. The mixed-load benchmark
measures the added cost of active subscriptions and message delivery.
