# Proposal: Built-in MCP Server

## Summary

Add a built-in [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server to Plasmite, exposing pool operations as MCP tools and pools as MCP resources. Two entry points: `plasmite mcp` for local stdio transport and a `/mcp` endpoint on the existing `plasmite serve` HTTP server.

## Experimental posture (v1)

This feature is explicitly experimental. The goal is broad MCP interoperability with the smallest viable surface area.

Design constraints for v1:

- Prefer simple, deterministic behavior over feature completeness.
- Reuse existing Plasmite auth/access/core paths; no new policy engine.
- Ship a narrow read/write tool set first; expand only after real usage feedback.
- Keep server behavior mostly stateless and avoid long-lived coordination machinery.

Known constraints in v1 (intentional):

- No server-side TTL/expiry for coordination messages; freshness is client-side policy.
- No built-in "latest per key" read mode; clients reconstruct current state from events.
- No atomic claim/check-and-set primitive; coordination remains advisory.
- `where` is jq-based; use tags for simple routing when possible.
- `plasmite_pool_list` exposes structural pool info only (no descriptions/prefix filtering yet).

## Motivation

Agents are processes. Plasmite is IPC for processes. MCP is the protocol agents use to discover and invoke tools.

Today, agents coordinate through the filesystem — temp files, lock files, scratch notes, polled directories. These are exactly the primitives Plasmite already replaces for regular processes. But agents can't use Plasmite without custom integration: writing shell commands via a coding tool, parsing output, handling errors manually.

An MCP server closes that gap. Any agent harness that speaks MCP (Amp, Claude Code, Cursor, Codex, Windsurf, Roo Code, and others) gets pool operations as native tools. Agents can feed, follow, and fetch without knowing anything about Plasmite's CLI — the harness handles tool discovery and invocation automatically.

### What this enables

- **Agent-to-agent coordination**: Agent A feeds `{"task": "auth-refactor", "status": "done"}` into a pool. Agent B reads the same pool. They coordinate through persistent messages without the harness mediating.
- **Agent memory across sessions**: An agent writes decisions and state to a pool. Next session, a new agent instance reads the pool and picks up context — bounded, structured, replayable.
- **Agent audit trail**: Every action an agent takes through MCP pools is a persistent, searchable event. Open the web UI and watch agents work.
- **Multi-agent file claims**: A `claims` pool where agents announce which files they're touching — the missing primitive for concurrent agents in one codebase.

## Design

### Two transports, one implementation

The MCP tool/resource definitions are shared. Only the transport layer differs.

#### `plasmite mcp` — stdio transport (local agents)

Speaks MCP-over-stdio: JSON-RPC 2.0 messages on stdin/stdout, newline-delimited. This is what most agent harnesses use for local tool servers.

Agent harness configuration:

```json
{
  "mcpServers": {
    "plasmite": {
      "command": "plasmite",
      "args": ["mcp"]
    }
  }
}
```

With a custom pool directory:

```json
{
  "mcpServers": {
    "plasmite": {
      "command": "plasmite",
      "args": ["mcp", "--dir", "/path/to/pools"]
    }
  }
}
```

`plasmite mcp` is a long-running process. The harness spawns it, negotiates capabilities, and invokes tools for the lifetime of the agent session.

#### `plasmite serve` — streamable HTTP transport (remote agents)

Adds a `/mcp` endpoint to the existing HTTP server. Same auth (bearer token), same TLS, same bind address. No second process, no second port.

```json
{
  "mcpServers": {
    "plasmite-remote": {
      "type": "streamable-http",
      "url": "https://server:9700/mcp"
    }
  }
}
```

The `/mcp` route follows the MCP streamable HTTP spec (2025-11-25), with an intentionally minimal experimental profile: POST JSON-RPC is implemented in v1, and GET returns `405 Method Not Allowed`.

The `/mcp` endpoint is outside the frozen v0 remote API surface (like `/ui` and `/healthz`), so it can evolve independently.

#### Streamable HTTP requirements (v1)

For interoperability with MCP clients, `/mcp` follows these wire-level rules:

- POST requests use `Accept: application/json, text/event-stream`.
- POST body is exactly one JSON-RPC message (request, notification, or response).
- For POST notifications/responses accepted by the server, return `202 Accepted` with no body.
- For POST requests in v1, return `Content-Type: application/json` with one JSON-RPC response (no POST-SSE mode in v1).
- GET requests return `405 Method Not Allowed` in v1 (no standalone SSE stream yet).
- Clients send `MCP-Protocol-Version` after initialization; unsupported/invalid values return `400 Bad Request`.
- When `Origin` is present, validate it and reject invalid origins with `403`.
- v1 is stateless over HTTP: no `MCP-Session-Id` requirements.

### MCP capabilities declared

```json
{
  "capabilities": {
    "tools": { "listChanged": false },
    "resources": { "subscribe": false, "listChanged": false }
  }
}
```

### Tools

Tools are the primary interface. Each maps directly to an existing core operation.

#### `plasmite_pool_list`

List available pools.

```json
{
  "name": "plasmite_pool_list",
  "description": "List all pools in the pool directory.",
  "inputSchema": {
    "type": "object",
    "properties": {}
  }
}
```

Returns: JSON array of pool info objects (name, path, capacity, bounds).

#### `plasmite_pool_create`

```json
{
  "name": "plasmite_pool_create",
  "description": "Create a new pool. Returns pool info on success.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "name": { "type": "string", "description": "Pool name" },
      "size": { "type": "integer", "description": "Pool size in bytes (default: 1048576)" }
    },
    "required": ["name"]
  }
}
```

#### `plasmite_pool_info`

```json
{
  "name": "plasmite_pool_info",
  "description": "Get metadata and metrics for a pool.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "pool": { "type": "string", "description": "Pool name" }
    },
    "required": ["pool"]
  }
}
```

#### `plasmite_pool_delete`

```json
{
  "name": "plasmite_pool_delete",
  "description": "Delete a pool.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "pool": { "type": "string", "description": "Pool name" }
    },
    "required": ["pool"]
  }
}
```

#### `plasmite_feed`

```json
{
  "name": "plasmite_feed",
  "description": "Append a JSON message to a pool. Returns the committed message envelope (seq, time, meta).",
  "inputSchema": {
    "type": "object",
    "properties": {
      "pool": { "type": "string", "description": "Pool name" },
      "data": { "description": "JSON message payload (object, array, string, number, etc.)" },
      "tags": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Optional tags for filtering"
      },
      "create": {
        "type": "boolean",
        "description": "Create the pool if it doesn't exist (default: false)"
      }
    },
    "required": ["pool", "data"]
  }
}
```

#### `plasmite_fetch`

```json
{
  "name": "plasmite_fetch",
  "description": "Fetch a single message by sequence number.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "pool": { "type": "string", "description": "Pool name" },
      "seq": { "type": "integer", "description": "Sequence number" }
    },
    "required": ["pool", "seq"]
  }
}
```

#### `plasmite_read`

Read recent or matching messages from a pool. This is the MCP equivalent of `follow --tail` / `follow --since` — a batch read, not a live stream. Agents call it, get messages, process them, call again if needed.

```json
{
  "name": "plasmite_read",
  "description": "Read messages from a pool. Returns up to `count` messages in ascending sequence order. Without `after_seq`, this returns the last `count` matching messages (still ascending). Use `since` for a time window, or `after_seq` to resume from a known position.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "pool": { "type": "string", "description": "Pool name" },
      "count": { "type": "integer", "description": "Max messages to return (default: 20, max: 200)" },
      "after_seq": { "type": "integer", "description": "Return messages after this sequence number (for pagination/resumption)" },
      "since": { "type": "string", "description": "Time window, e.g. '5m', '1h', '2024-01-15T00:00:00Z'" },
      "tags": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Filter by tags"
      },
      "where": { "type": "string", "description": "jq predicate for filtering, e.g. '.data.status == \"done\"'" }
    },
    "required": ["pool"]
  }
}
```

The `after_seq` parameter is the key to agent workflows that poll: read once, note the last seq, read again later with `after_seq` to get only new messages. This avoids the need for live streaming in the MCP tool model.

Ordering rules:

- Results are always returned in ascending `seq` order.
- If `after_seq` is set, return messages where `seq > after_seq`.
- If `after_seq` is not set, return the last `count` matching messages, still in ascending `seq` order.
- If both `since` and `after_seq` are set, apply both filters (intersection): messages must satisfy the time window and `seq > after_seq`.

### Resources

Pools are exposed as MCP resources for passive context. An agent (or its harness) can read a pool resource for recent state and poll again as needed.

Resource URIs follow the pattern `plasmite:///pools/{name}`.

#### Resource list

`resources/list` returns one resource per pool:

```json
{
  "uri": "plasmite:///pools/events",
  "name": "events",
  "description": "Plasmite pool: events (seq 1–4207, 1.0 MB)",
  "mimeType": "application/json"
}
```

#### Resource read

`resources/read` for a pool URI returns the most recent messages (default 20) as MCP resource contents. This gives agents passive context without invoking a tool.

Shape:

```json
{
  "contents": [
    {
      "uri": "plasmite:///pools/events",
      "mimeType": "application/json",
      "text": "{\"messages\":[...],\"next_after_seq\":4207}"
    }
  ]
}
```

`text` contains UTF-8 JSON with ascending messages and a `next_after_seq` cursor.

#### Resource subscriptions

Subscriptions and resource change notifications are intentionally out of scope for experimental v1. Clients poll via `plasmite_read` (`after_seq`) or call `resources/read` again.

### What `follow` is not

MCP tools are request/response. A tool cannot stream indefinitely. `plasmite_read` returns a batch and the agent polls with `after_seq` if it wants to keep up. This is the right fit for agents, which operate in think→act→observe cycles, not continuous stream processing.

For live streaming use cases (dashboards, log tailing), the existing CLI, HTTP API, and web UI remain the right tools.

## Architecture

### Layer placement

The MCP server is an interface adapter, like the HTTP server and CLI. It invokes core operations through `src/api` and `src/core`. No new correctness logic.

```
MCP stdio ─┐
            ├─► MCP handler (shared) ─► src/api ─► src/core
MCP HTTP ──┘
```

### Module structure

```
src/
  mcp.rs              — MCP tool/resource definitions, JSON-RPC dispatch
  mcp_stdio.rs        — stdio transport (`plasmite mcp`)
  serve.rs            — adds /mcp route using mcp.rs handlers
```

`mcp.rs` is transport-agnostic: it takes JSON-RPC requests and returns JSON-RPC responses. The transport modules handle framing (newline-delimited stdio vs HTTP POST JSON in v1).

### Dependencies

The MCP protocol is JSON-RPC 2.0 over simple transports. No MCP SDK dependency is needed — the protocol is small enough to implement directly with `serde_json` and the existing Axum infrastructure. This avoids pulling in a Node/TypeScript SDK or a heavy Rust crate for what amounts to ~10 JSON-RPC methods.

If a mature, minimal Rust MCP SDK emerges and proves worthwhile, it can be adopted later. The tool/resource definitions in `mcp.rs` would remain the same.

### Auth

- **stdio**: No auth. The agent harness spawned the process; trust is implicit (same as any stdio MCP server).
- **HTTP**: Reuses existing `plasmite serve` bearer token auth. The `/mcp` endpoint checks `Authorization: Bearer <token>` identically to `/v0/pools/*`. Agent harnesses that support streamable HTTP auth pass the token in their MCP config.

### Error mapping

MCP tool calls use two error paths:

1. **Protocol errors** (JSON-RPC `error` object):
   - Unknown tool name
   - Malformed `tools/call` request
   - Transport/protocol failures

2. **Tool execution errors** (JSON-RPC success + `result.isError: true`):
   - Pool not found
   - Access-mode denied
   - Already exists / busy / corrupt / IO failures
   - Input and business-rule validation failures inside a valid tool call

Plasmite maps domain `ErrorKind` values to tool execution errors for `tools/call`, and returns actionable text plus structured fields (for example `kind`, `message`, `pool`, `seq`) so clients/LLMs can recover.

## CLI surface

### New subcommand

```
plasmite mcp [--dir DIR]
```

Starts an MCP server on stdio. Runs until stdin closes or the client sends a shutdown.

### Changes to `serve`

The `/mcp` endpoint is added unconditionally when `plasmite serve` runs. No new flags required. It participates in existing `--access` mode restrictions:

- `read-only`: `plasmite_feed`, `plasmite_pool_create`, `plasmite_pool_delete` return 403-equivalent JSON-RPC errors.
- `write-only`: `plasmite_read`, `plasmite_fetch`, `plasmite_pool_info`, `plasmite_pool_list` return 403-equivalent errors.
- `read-write` (default): all tools available.

A `--no-mcp` flag can suppress the endpoint if needed, but the default is on.

## Scope boundaries

### In scope

- MCP tool definitions for pool CRUD, feed, fetch, and batch read.
- MCP resource definitions for pool list/read.
- stdio and streamable HTTP transports.
- Reuse of existing auth, TLS, access-mode, and core API infrastructure.

### Out of scope

- **MCP Prompts**: No templated prompt definitions. Plasmite is a data layer, not a prompt source.
- **MCP Sampling**: No server-initiated LLM calls. Plasmite never calls an LLM.
- **Live streaming tools**: MCP tools are request/response. Streaming stays in CLI/HTTP/UI.
- **Resource subscriptions + change notifications**: No `resources/subscribe` or `notifications/resources/*` in experimental v1.
- **Agent-specific conventions in protocol**: Pool naming and claim policy are not encoded in MCP methods, but we do document a default convention in cookbook/agent instructions for launch.
- **MCP SDK dependency**: Implement the protocol directly; adopt an SDK later if one proves valuable.
- **HTTP session management**: No `MCP-Session-Id` state machine in v1.
- **SSE resumability/replay**: No `Last-Event-ID` handling in v1.

## Testing

- **Unit tests**: JSON-RPC dispatch in `mcp.rs` — valid requests, invalid params, error mapping.
- **Integration tests (stdio)**: Spawn `plasmite mcp` as a child process, send JSON-RPC on stdin, validate responses on stdout. Test tool invocations end-to-end: create pool → feed → read → fetch → delete.
- **Integration tests (HTTP)**: Same tool invocations via HTTP POST to `/mcp`.
- **Conformance**: Validate against the MCP Inspector (`npx @modelcontextprotocol/inspector`) for protocol compliance.

## Cookbook additions

After implementation, add a "Agent Coordination" section to `docs/cookbook.md` showing:

- Agent harness config for `plasmite mcp`.
- Two agents coordinating through a shared pool.
- Using `plasmite_read` with `after_seq` for polling new messages.
- Agent writing structured status updates to a pool.
- Watching agent activity in the web UI.
- Preferred claims pattern using tags (`claim`, `agent:<id>`, `file:<path>`) over jq for file lookups.
- Explicit note that stale claims are filtered by a time window in clients (for example `since: "10m"`).

## v1 policy decisions

1. **Tool names**: Use `plasmite_*` prefix for collision safety.
2. **Read cap**: `plasmite_read` max batch size is fixed at 200 in v1 (not configurable).
3. **Read ordering**: All reads return ascending `seq`; polling is `after_seq`-based.
4. **HTTP transport shape**: POST JSON request/response only in v1; GET `/mcp` returns 405.
5. **Resources behavior**: `resources/list` + `resources/read` only; no subscriptions/notifications in v1.
6. **Coordination semantics**: claims are advisory and non-atomic in v1.
7. **Claims freshness**: no server TTL; clients apply time-window filtering.
8. **Pool discovery scope**: pool list is intentionally minimal (no description/prefix query in v1).

## Post-v1 candidates (based on field usage)

- `plasmite_feed` optional `ttl` lease expiry for coordination messages.
- `plasmite_read` optional `latest_by` / `distinct_by` selector for state reconstruction.
- Optional atomic coordination helper (claim-if-clear or compare-and-swap style).
- Pool metadata on create/list (`description`, optional ownership/purpose fields).
- Optional pool list prefix filter for large/shared directories.
- Optional "block until match" read helper (polling remains baseline).

## Implementation checklist (v1)

1. **Protocol skeleton**
   - Implement `initialize`, `initialized`, `ping`, `tools/list`, `tools/call`, `resources/list`, `resources/read`.
   - Enforce JSON-RPC shape and request/notification semantics.
2. **Transport compliance**
   - `plasmite mcp`: newline-delimited JSON-RPC over stdio.
   - `/mcp`: POST JSON behavior, `Accept` negotiation, `202` semantics, `MCP-Protocol-Version` handling, GET returns 405.
   - Validate `Origin` on HTTP requests.
3. **Tool surface**
   - Implement all proposed `plasmite_*` tools with JSON Schema validation.
   - Ensure `plasmite_read` ordering/cursor behavior is deterministic (ascending + `after_seq`).
4. **Resource surface**
   - Expose pool resources at `plasmite:///pools/{name}`.
   - Return `resources/read` in MCP `contents[]` shape.
   - No subscriptions/notifications in v1.
5. **Auth + access mode**
   - Reuse existing bearer-token auth and `--access` gating for `/mcp`.
   - Keep stdio unauthenticated.
6. **Error semantics**
   - Use JSON-RPC errors for protocol issues.
   - Use `result.isError: true` for tool execution failures with actionable messages.
7. **Tests**
   - Unit tests for request routing, schema validation, and error mapping behavior.
   - Integration tests for stdio and HTTP transports (CRUD + feed/read/fetch + access-mode denials).
   - Inspector-based conformance sanity check.
8. **Docs**
   - Add cookbook section for agent coordination and polling with `after_seq`.
   - Add serving docs note for `/mcp` transport semantics and required headers.
