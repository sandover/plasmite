# MCP client connections

November 2026 · Plasmite `2.0.0-dev` on macOS. These checks show that two agent hosts connect with their default protocol settings. They don't cover other hosts, other platforms or later client versions.

## What passed

- **Claude Code 2.1.323** listed pools and read the latest message from a named pool over stdio and over HTTPS.
- **Codex CLI 0.171.0-alpha.13** did the same over stdio and over HTTPS. A separate run also signed in and read with only the `/mcp` URL configured.

Both hosts used their default MCP protocol settings. For HTTPS, the server used a disposable test certificate authority and access key, and a script submitted the browser approval form.

## Repeat the check

Follow the [agent setup guide](../agents.md#connect-an-agent), then ask the agent to list pools and read the latest message from a pool you can name. `just mcp-smoke` checks the protocol itself against the pinned Python SDK for both MCP revisions; see [testing](../record/testing.md).
