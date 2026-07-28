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
│
│   Design audits and proposals
├── proposals/cli-help-system.md               — CLI/help audit and reform model; read before redesigning command discovery
├── proposals/mcp-server.md                    — MCP design history; read when revisiting the MCP surface
│
│   Docs of record
├── record/README.md                           — Docs of record index; start here for stable policies and runbooks
├── record/vision.md                           — Product scope + principles; read when breaking scope ties
├── record/architecture.md                      — Implementation architecture; read when changing internals or layering
├── record/testing.md                           — Test strategy + commands; read when adding/fixing tests
├── record/releasing.md                         — Release policy + versioning; read for what/why (mechanics live in release skill)
├── record/distribution.md                      — Supported platforms, install channels, and SDK layout; read when adding a channel or platform
├── ../include/plasmite.h                       — C ABI header; read for stability contract, ownership rules, linking
├── record/serving.md                           — Serving + remote access (TLS, auth, CORS, deployment)
│
└── images/ui/                                  — UI screenshots; read when updating docs/UI references

spec/
├── README.md                                   — Spec index; start here for contract navigation
├── v0/SPEC.md                                  — Command-line interface (CLI) contract; read before changing CLI behavior
├── api/v0/SPEC.md                               — Public API contract; read before changing the API surface
└── remote/v0/SPEC.md                            — Remote protocol contract; read before changing HTTP endpoints/semantics
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

# Guidance
- no pip in this project -- uv only
