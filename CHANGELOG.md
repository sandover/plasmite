# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

## [0.1.0] - 2026-02-06

### Added
- `plasmite serve` - HTTP/JSON server with TLS + token auth
- `plasmite doctor` - Pool validation and diagnostics
- Language bindings: Go, Python, Node.js (via libplasmite C ABI)
- Public Rust API (`plasmite::api`)
- Remote protocol spec (HTTP/JSON)
- Conformance test suite (cross-language)
- Remote `feed`/`follow` via shorthand URLs
- Inline seq→offset index for fast `get(seq)` lookups
- `follow --where` filtering with jq predicates
- `follow --tag` filtering with exact tag match
- `follow --replay` for timed playback at configurable speeds
- `follow --one`, `follow --timeout`, `follow --data-only` for scripting
- Shell completion (bash/zsh/fish)
- Web UI (zero-build single-page app) at `/ui`
- Binary releases for macOS (arm64/amd64) and Linux (amd64/arm64)

### Changed
- Pool format: added inline index region (requires pool recreation from v0.0.1)
- Improved CLI help text and error messages
- Performance: 600k+ msg/sec append, sub-ms follow latency

## [0.0.1] - 2026-01-30

### Added
- Initial CLI implementation for local pools.
- Homebrew tap instructions and release workflow.
- CI for formatting, clippy, and tests on Linux/macOS.
- Bench suite for local performance baselines.
- Performance baseline documentation.

### Changed
- Stable JSON-first CLI output and help text, with improved UX defaults.
- Pinned metadata for v0.0.1 and clarified CLI-only scope.
