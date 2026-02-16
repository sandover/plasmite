This repository inherits global agent policy from `/Users/brandonharvey/AGENTS.md`.
Keep this file limited to plasmite-specific deltas.

## Project Docs Map
```
docs/
├── README.md                                  — Docs index; start here if you don't know what you need yet
│
│   Top-level reference
├── building.md                                — Build system + vendoring; read when touching build/release tooling
├── cookbook.md                                — Task-oriented examples; read when you want copy/paste CLI workflows
│
│   Docs of record (canonical, current truth)
├── record/README.md                           — Docs of record index; start here for stable policies and runbooks
├── record/vision.md                           — Product scope + principles; read when breaking scope ties
├── record/architecture.md                      — Implementation architecture; read when changing internals or layering
├── record/testing.md                           — Test strategy + commands; read when adding/fixing tests
├── record/releasing.md                         — Release policy + versioning; read for what/why (mechanics live in release skill)
├── record/distribution.md                      — Supported platforms, install channels, and SDK layout; read when adding a channel or platform
├── ../include/plasmite.h                       — C ABI header; read for stability contract, ownership rules, linking
├── record/serving.md                           — Serving + remote access (TLS, auth, CORS, deployment)
│
│   Planning (in-flight; promote to record/decisions when accepted)
├── planning/README.md                          — Planning index; start here for active proposals/roadmap notes
│
├── decisions/README.md                         — Catalog of Architecture Decisions in decisions/
│
│   Assets
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
- Before push, run and pass `just ci-fast`; before merge/release, run and pass `just ci`.
- Do not add new `#[allow(clippy::...)]` without explicit justification in the commit body.

# Guidance
- no pip in this project -- uv only
