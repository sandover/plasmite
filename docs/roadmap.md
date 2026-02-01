<!--
Purpose: Track planned capabilities and sequencing without turning it into a binding spec.
Exports: N/A (documentation).
Role: Product/engineering roadmap (non-normative); links to ADRs for design details.
Invariants: v0 contract remains stable; roadmap items are non-binding until promoted into a spec version.
-->

# Roadmap

This roadmap is outcome-oriented. Design details live in ADRs; compatibility promises live in versioned specs.

## After v0.0.1

- Remote pool refs + `plasmite serve` (TCP first)
- Per-entry checksums (opt-in; not in v0.0.1)
- Pattern matching / filtering interface (stateless; interface TBD)
- Shell completion
- `plasmite doctor` (validation + diagnostics)

## Later

- QUIC transport (“UDP access” via QUIC streams + TLS)

