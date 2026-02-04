<!--
Purpose: Record the decision to prototype a remote protocol using HTTP/JSON with token auth.
Exports: N/A (documentation).
Role: ADR (Accepted) for post-v0 remote access direction.
Invariants: Local-first API remains the primary contract in v0.
Notes: Details may evolve during implementation, but the default UX stays HTTP/JSON.
-->

# ADR 0004: Remote protocol direction (HTTP/JSON + bearer tokens)

- Date: 2026-02-04
- Status: Accepted

## Context

Plasmite is local-first in v0, but a remote transport is on the roadmap. We need a
human-friendly, low-friction path for users and tooling that aligns with common
infrastructure and security practices.

## Decision

1. Prototype remote access over **HTTPS with JSON request/response bodies**.
2. Use **bearer token authentication** as the default auth model in the initial design.
3. Keep the protocol additive and transport-agnostic so other transports (e.g. QUIC)
   can map onto the same semantics later.

## Consequences

- Users can interact with a remote Plasmite service using standard tools (curl, browser, SDKs).
- Deployments can rely on existing TLS termination and reverse proxies.
- Auth can be extended later (mTLS, OAuth) without breaking the base protocol.
- The initial spike will focus on API shape and UX, not performance tuning.
