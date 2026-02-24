# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

## [0.5.0] - 2026-02-24

### Added
- `plasmite duplex` — read and write a pool from one command. TTY mode wraps each line as `{"from": ME, "msg": LINE}` for live chat; non-TTY mode ingests a JSON stream. Supports `--tail`, `--since`, `--timeout`, `--echo-self`, and remote pools.
- Subcommands now show their help text when required arguments are missing.

### Changed
- CI and release pipeline simplified (-636 lines); consolidated workflow topology.
- Vision and architecture docs deepened into governing documents of record.

## [0.4.0] - 2026-02-18

### Added
- SDK-grade typed APIs across Go, Python, and Node bindings (ergonomic round 2).
- Pool directories are auto-created on `create_pool` across all bindings.
- cargo-binstall preview channel for Linux/macOS.

### Changed
- Documentation indexes and binding READMEs audited for API/default/command accuracy.
- Corrected Go module path to `github.com/sandover/plasmite/bindings/go` so downstream `go get github.com/sandover/plasmite/bindings/go/local` resolves from this repo layout.
- Simplified shared internals: CLI dispatch split into `src/command_dispatch.rs`, pool path/info helpers centralized, and serve tail setup deduplicated.
- Simplified binding maintenance: Node error/type-surface mapping centralized with a declaration drift gate, binding tests use reusable setup helpers, and conformance runners share per-language step-dispatch/pool-open helpers.

## [0.3.0] - 2026-02-16

### Added
- Deterministic cookbook smoke coverage in CI hardening lanes.
- Expanded remote and CLI hardening/security negative-test coverage.

### Changed
- Completed CLI naming migration to `feed` / `follow` / `fetch`.
- Go bindings package layout now uses `bindings/go/api` (pure contracts) and `bindings/go/local` (cgo implementation); import paths changed without a compatibility shim.
- README content was rewritten around real-world use cases and performance framing.

## [0.2.0] - 2026-02-15

### Added
- Windows (`x86_64-pc-windows-msvc`) npm and PyPI install support.
- CI coverage and release plumbing for Windows artifact smoke paths.
- Browser CORS allowlist ergonomics and serving guidance improvements.

### Changed
- Release artifacts now include Windows import-library support needed by bindings.
- Documentation was consolidated into canonical docs-of-record and decision docs.

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
