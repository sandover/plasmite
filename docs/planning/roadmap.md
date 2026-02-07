<!--
Purpose: Track active and near-term planning priorities without creating normative compatibility promises.
Exports: N/A (documentation).
Role: Living planning artifact for product and engineering sequencing.
Invariants: Items here are directional only until captured in specs/ADRs or promoted into docs-of-record.
-->

# Roadmap

This roadmap lists active and near-term priorities.
It is intentionally short and should only include work that is currently relevant.

## Active now

- Distribution and release channel setup
  - first public release versioning decision
  - CI release workflow for binary artifacts
  - Homebrew tap publishing path

## Next

- Binary payload conventions (inline blobs/chunking)
- Windows platform support milestones

## Later (candidate)

- Optional per-entry checksums
- Optional `.NET` bindings
- Additional remote transport work beyond HTTP/JSON (for example QUIC)
