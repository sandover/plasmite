This repository adds Plasmite-specific instructions to the shared global policy
in `~/.config/AGENTS.md`.

## Project Docs Map
```
docs/
├── README.md                                  — Docs index; start here if you don't know what you need yet
│
│   Top-level reference
├── building.md                                — Build system + vendoring; read when touching build/release tooling
├── cli.md                                     — CLI operating model; read when resolving pool refs, input/output modes, or exits
├── cookbook.md                                — Task-oriented examples; read when you want copy/paste CLI workflows
├── performance/transport-comparison.md        — Measured transport throughput; read when comparing native API and MCP paths
├── performance/1.0-release.md                  — Read when assessing 1.0 read costs, writer contention, or benchmark results
├── performance/windows-access-1.0.md          — Read when comparing local, HTTP, and MCP access costs on Windows
├── performance/consumer-latency.rs             — Read when reproducing the full-consumer latency measurements
│
│   Design audits and proposals
├── proposals/cli-help-system.md               — CLI/help audit and reform model; read before redesigning command discovery
├── proposals/cli-surface.md                   — Proposed command surface; read when reviewing CLI naming, shared rules, or help
├── proposals/mcp-server.md                    — MCP design history; read when revisiting the MCP surface
├── proposals/serve-mcp-native.md              — Secure sharing; read when designing access keys, native connections, or MCP authorization
├── proposals/serve-access-mvp.md              — Earlier access proposal; read when tracing the preceding invitation design
│
│   Docs of record
├── record/README.md                           — Docs of record index; start here for stable policies and runbooks
├── record/vision.md                           — Product scope + principles; read when breaking scope ties
├── record/architecture.md                      — Implementation architecture; read when changing internals or layering
├── record/testing.md                           — Test strategy + commands; read when adding/fixing tests
├── record/releasing.md                         — Release policy + versioning; read for what/why (mechanics live in release skill)
├── record/upgrading-1.0.md                     — Read when updating scripts, remote configuration, or Rust clients to 1.0
├── record/distribution.md                      — Supported platforms, install channels, and SDK layout; read when adding a channel or platform
├── ../include/plasmite.h                       — C ABI header; read for stability contract, ownership rules, linking
├── record/windows-boot-validation.md           — Read when assessing native Windows service checks and their support boundary
├── record/serving.md                           — Read when sharing pools, setting browser trust, or operating a secure server
│
├── images/README.md                            — Read when reproducing README diagrams or terminal/browser recordings
└── images/ui/                                  — UI screenshots; read when updating docs/UI references

spec/
├── README.md                                   — Spec index; start here for contract navigation
├── v0/SPEC.md                                  — Command-line interface (CLI) contract; read before changing CLI behavior
├── api/v0/SPEC.md                               — Public API contract; read before changing the API surface
├── remote/v0/SPEC.md                            — Remote protocol contract; read before changing HTTP endpoints/semantics
└── mcp/2025-11-25/SPEC.md                       — MCP contract; read before changing local or direct MCP behavior
```

## Maintaining the Docs Map
When you add, rename, move, or delete a doc in `docs/` or `spec/`, update the tree above.

- Descriptions must answer “read this when…”, not restate the filename.
- Spell out acronyms on first use.

# CI hygiene (required before pushing code)
- Run `cargo fmt --all`.
- Run `cargo clippy --all-targets -- -D warnings`.
- Before push, run and pass `just check`; before merge/release, run and pass `just release-gate`.
- Do not add new `#[allow(clippy::...)]` without explicit justification in the commit body.

# Build hygiene
- Keep current debug artifacts during active development.
- Ensure scratch directories are removed after successful tests.
- Run `cargo clean` after release work. `just check` cleans `target/` itself when it exceeds 2 GB.
- Preserve release evidence, active Ergo state, and all source changes during cleanup.

# Guidance
- no pip in this project -- uv only
