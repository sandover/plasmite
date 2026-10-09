# Plasmite docs

Start with the task you want to do:

| I want to… | Read |
| --- | --- |
| Send and read messages on one machine | [Cookbook](cookbook.md) |
| Share a pool with another machine | [Share your first pool](record/serving.md#share-your-first-pool) |
| Share through a Tailscale network | [Tailscale setup](record/serving.md#use-tailscale) |
| Use shared pools in a browser | [Browser setup](record/serving.md#open-pools-in-a-browser) |
| Give Claude Code or Codex CLI access to pools | [AI client setup](record/serving.md#connect-an-ai-client) |
| Revoke access or remove a saved connection | [Manage access](record/serving.md#manage-access) |
| Run a server with my own certificate or proxy | [Deploy a server](record/serving.md#deploy-a-server) |
| Fix a connection | [Troubleshooting](record/serving.md#troubleshoot-a-connection) |
| Upgrade from an earlier release | [Upgrade to 1.0](record/upgrading-1.0.md) |
| Install Plasmite | [Install channels](record/distribution.md#install-matrix) |
| Install on Raspberry Pi or Linux ARM | [Published ARM SDK installation (preview)](record/distribution.md#linux-arm-sdk-preview-install) |
| Build from this checkout | [Source installation](building.md#install-the-cli-from-source) |

## Guides and reference

- [CLI guide](cli.md): pool references, input, output, and exit behavior.
- [Serving guide](record/serving.md): secure sharing, browser trust, server
  operation, and recovery.
- [Distribution](record/distribution.md): supported platforms, install
  channels, and software development kit (SDK) layout.
- [Transport measurements](performance/transport-comparison.md): compare
  native and Model Context Protocol (MCP) performance.
- [Windows access measurements](performance/windows-access-1.0.md): compare
  local libraries, command-line tools, HTTP, and MCP in Plasmite 1.0.0.
- [1.0 performance measurements](performance/1.0-release.md): measured writer
  improvements, small-read costs and consumer latency on one Mac.

For exact contracts, start with the [specifications](../spec/README.md):

- [CLI](../spec/v0/SPEC.md)
- [Public API](../spec/api/v0/SPEC.md)
- [Remote HTTP protocol](../spec/remote/v0/SPEC.md)
- [MCP](../spec/mcp/2025-11-25/SPEC.md)
- [C interface and ownership rules](../include/plasmite.h)

## Work on Plasmite

The [docs of record](record/README.md) describe the current design and
policies. Start with [vision](record/vision.md) for product scope and
[architecture](record/architecture.md) for internal structure. Use
[building](building.md), [testing](record/testing.md), and
[releasing](record/releasing.md) when changing or shipping code.

Proposals record design history. Current setup and behavior live in the guides
and specifications above:

- [Secure sharing design history](proposals/serve-mcp-native.md)
- [Earlier invitation design](proposals/serve-access-mvp.md)
- [MCP server design](proposals/mcp-server.md)
- [CLI and help audit](proposals/cli-help-system.md)
- [Proposed CLI surface design history](proposals/cli-surface.md)
- [UDP input design](proposals/udp-input.md): read when designing a small
  datagram input for an existing pool.

The repository's `.ergo/` backlog and journal track planned work and results.
