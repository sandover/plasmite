# Specs

Specs hold the contracts that users and other programs rely on. The 2.0 entries describe the current release; the 1.x entries describe published 1.x releases.

## Plasmite 2.0

- [2.0 contract](v2/SPEC.md): read before changing the CLI, libraries, HTTP, UDP or Model Context Protocol (MCP) behavior.
- [UDP transport v1](udp/v1/SPEC.md): read when changing UDP feed and follow, subscription controls or delivery events. 2.0 UDP follows this spec.
- [UDP input v0](udp/v0/SPEC.md): read when changing the UDP datagram format or listener. 2.0 UDP keeps this wire format.

## Plasmite 1.x

- [CLI contract](v0/SPEC.md): read when checking how 1.x commands behaved.
- [Public API contract](api/v0/SPEC.md): read when checking 1.x library behavior.
- [HTTP remote protocol](remote/v0/SPEC.md): read when checking the 1.x HTTP routes. The 2.0 contract still uses its access rules and server limits.
- [MCP contract](mcp/2025-11-25/SPEC.md): read when checking MCP lifecycle, transports and authorization, which 2.0 keeps.

## What belongs in a spec

Specs capture guarantees that callers outside the codebase rely on:

- Versioning and the policy for additive and breaking changes.
- Stable names and the shapes of commands, endpoints and calls, including tables that map one operation to its name in each language.
- Data, ordering and error invariants.
- Surfaces that sit outside the contract.

Specs leave out:

- Full language-level signatures; code-level docs hold those.
- Option-by-option reference, unless the contract depends on it.
- Implementation notes, internal architecture and long examples.
