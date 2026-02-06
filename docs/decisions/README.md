<!--
Purpose: Explain how architectural/product decisions are recorded for Plasmite.
Exports: N/A (documentation).
Role: Process doc; points contributors at ADRs and how to add new ones.
Invariants: ADRs are immutable once accepted; superseding decisions use “Supersedes:” trailers.
-->

# Decisions (ADRs)

Plasmite records architectural/product decisions as ADRs under `docs/decisions/`.

## Rules

- One decision per file.
- Use the next available number (`0001-...`).
- Keep it short: context → decision → consequences.
- Use **Status: Proposed** when still exploring.
- If a decision changes, write a new ADR that supersedes the old one.

## Current ADRs

- `0001-transport-strategy-tcp-now-quic-later.md`
- `0002-per-entry-checksums-not-in-v0.md`
- `0003-pattern-matching-interface-deferred.md`
- `0004-remote-protocol-http-json.md`
- `0005-inline-seq-index.md`
