<!--
Purpose: Record the decision to deliver remote access via TCP first and QUIC later, without baking transport details into the CLI contract.
Exports: N/A (documentation).
Role: ADR (Accepted).
Invariants: Core operations remain transport-agnostic; framing is shared across transports.
-->

# ADR 0001: Transport strategy (TCP now, QUIC later)

- Date: 2026-02-01
- Status: Accepted

## Context

Plasmite is local-first today, but we want to prepare for remote access in a clean and elegant way.
There is interest in TCP and “UDP access”.

## Decision

1) Deliver remote access over **TCP** first.

2) Treat “UDP access” as **QUIC later** (UDP-based transport that provides reliable streams and TLS).

3) Keep the CLI contract **transport-agnostic**:
- Standardize a `PoolRef` abstraction (local now; URI schemes later).
- Define a shared framing format so transports are adapters, not separate protocols.

## Consequences

- `plasmite serve` can be thin: socket ↔ framing ↔ core ops.
- QUIC can be added without changing the message contract.
- We avoid designing a bespoke unreliable UDP protocol (ordering/backpressure/integrity pitfalls).

