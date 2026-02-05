<!--
Purpose: Track planned capabilities and sequencing without turning it into a binding spec.
Exports: N/A (documentation).
Role: Product/engineering roadmap (non-normative); links to ADRs for design details.
Invariants: v0 contract remains stable; roadmap items are non-binding until promoted into a spec version.
-->

# Roadmap

This roadmap is outcome-oriented. Design details live in ADRs; compatibility promises live in versioned specs.

## Completed (post v0.0.1)

- ✓ `plasmite serve` - HTTP/JSON server (loopback by default; non-loopback opt-in)
- ✓ `plasmite doctor` - Pool validation and diagnostics
- ✓ Language bindings - Go, Python, Node.js (via libplasmite C ABI)
- ✓ Public Rust API (`plasmite::api`)
- ✓ Remote protocol spec (HTTP/JSON)
- ✓ Conformance test suite (cross-language)

## In progress

- Shell completion
- Remote pool refs in CLI (using `plasmite serve` as backend)

## Future

- Per-entry checksums (opt-in)
- Binary payload conventions (inline blobs/chunking) + optional blob store
- Pattern matching / filtering interface (stateless; interface TBD)
- QUIC transport ("UDP access" via QUIC streams + TLS)
