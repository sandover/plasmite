This repository adds Plasmite-specific instructions to the shared global policy
in `~/.config/AGENTS.md`.

## Project Docs Map
```
docs/
├── README.md                                  — Docs index; start here if you don't know what you need yet
│
│   Top-level reference
├── building.md                                — Build system + vendoring; read when touching build/release tooling
├── cli.md                                     — CLI guide; read when predicting how reads start, gaps, input/output formats, or exit codes
├── cookbook.md                                — Task-oriented examples; read when you want copy/paste CLI workflows
├── agents.md                                  — Read when connecting AI agents through MCP or designing agent handoffs
├── performance/README.md                      — Read when choosing a performance report or measuring a change
├── performance/udp-transport.md               — Read when assessing UDP versus HTTP delivery, measured latency, or maximum datagram support
├── performance/udp-transport-bench.rs          — Read with the UDP transport report for the historical benchmark and its matching checkout
├── performance/udp-transport-results/          — Read when checking the raw runs and host setup behind the UDP measurements
├── performance/transport-comparison.md        — Measured transport throughput; read when comparing native API and MCP paths
├── performance/1.0-release.md                  — Read when assessing 1.0 read costs, writer contention, or benchmark results
├── performance/windows-access-1.0.md          — Read when comparing local, HTTP, and MCP access costs on Windows
├── performance/consumer-latency.rs             — Read when reproducing the full-consumer latency measurements
├── performance/language-interfaces.md          — Read when choosing owned views, native copies or explicit batches from measured complete paths
├── performance/language-boundary-results/    — Read when reproducing per-binding ownership and conversion measurements
│   ├── owned-views/README.md                 — Read when comparing owned views, local/UDP streams, forwarding, and runtime costs
│   ├── owned-views/python/README.md          — Read when comparing Python views, native input, owned UDP streams, or wheel coverage
│   │   ├── batches/README.md                — Read when comparing explicit Python batches, forwarding, paced costs, or owner retention
│   │   └── callback-guards/README.md        — Read when checking Python callback closure and physical byte-buffer lengths
│   ├── owned-views/go/README.md              — Read when assessing Go snapshot ownership, typed decoding, sizing, or cgo costs
│   ├── owned-views/go/streams/README.md       — Read when measuring Go view-stream ownership, delivery, and forwarding
│   ├── node-views/README.md                  — Read when assessing JavaScript view access, conversion costs, and native ABI checks
│   ├── portable-views/README.md              — Read when measuring portable Node startup, sparse/full work, copies, or retained WebAssembly (Wasm) memory
│   ├── http-views/README.md                  — Read when comparing HTTP owned views, retained append encodings, receipts, and client/server CPU
│   ├── c-json/README.md                      — Read when assessing direct JSON reads and stored-float correctness
│   ├── wakeups/README.md                     — Read when assessing shared-address waits and independent-reader wakeups
│   ├── append-descriptors/README.md           — Read when assessing borrowed append locking and removed descriptor calls
│   ├── encoding/README.md                     — Read when comparing parse-once encoding and structural size estimates
│   ├── append-receipts/README.md              — Read when evaluating numeric receipts versus complete producer results
│   ├── core-binary/README.md                  — Read when comparing binary validation and writer-handle measurements
│   └── node-owned/README.md                   — Read when evaluating raw buffer transfer and JSON compaction
├── performance/pipe-comparison.md              — Read when comparing a Python pipe with ordinary Plasmite calls
├── performance/pipe-comparison-results/         — Read when checking the raw pipe runs and append timing evidence
│
│   Docs of record
├── record/README.md                           — Docs of record index; start here for stable policies and runbooks
├── record/vision.md                           — Product scope + principles; read when breaking scope ties
├── record/architecture.md                      — Implementation architecture; read when changing internals or layering
├── record/testing.md                           — Test strategy + commands; read when adding/fixing tests
├── record/releasing.md                         — Release policy + versioning; read for what/why (mechanics live in release skill)
├── record/upgrading-2.0.md                    — Read when moving any 1.x application, script, HTTP or MCP client to 2.0
├── record/distribution.md                      — Supported platforms, install channels, and SDK layout; read when adding a channel or platform
├── ../include/plasmite.h                       — C ABI header; read for stability contract, ownership rules, linking
├── record/serving.md                           — Read when sharing pools, setting browser trust, or operating a secure server
│
│   Dated platform checks
├── validation/README.md                       — Read when finding dated platform checks and their support limits
├── validation/mcp-clients.md                   — Read when checking which MCP clients and versions connected over stdio and HTTPS
├── validation/mcp-wake-up.md                   — Read when evaluating resource notifications, sequence catch-up or agent wake-up evidence
├── validation/mcp-wake-up/                     — Run the disposable subscription fixture for the evidence above
├── validation/windows-boot-validation.md           — Read when assessing Windows service and macOS LaunchDaemon checks and their limits
├── validation/debian-vm-validation/               — Read when inspecting collector code and raw VM observations
├── validation/debian-vm-validation.md              — Read when checking Linux service startup, reboot recovery, failed writes, and sensor guidance
├── validation/arm-sdk-ci.md                        — Read when checking what the first Linux ARM SDK CI run established
├── validation/dependency-audit-2026-02-08.md       — Read when reviewing the February 2026 dependency and supply-chain risk audit
│
├── images/README.md                            — Read when reproducing README diagrams or terminal/browser recordings
└── images/ui/                                  — UI screenshots; read when updating docs/UI references

spec/
├── README.md                                   — Spec index; start here for contract navigation
├── v2/SPEC.md                                   — Current contract; read before changing the CLI, API, HTTP, UDP, or Model Context Protocol
├── v0/SPEC.md                                  — 1.x CLI contract; read when checking migration behavior
├── udp/v0/SPEC.md                              — User Datagram Protocol (UDP) input contract; read when changing UDP messages or listener behavior
├── udp/v1/SPEC.md                              — Read when changing UDP feed/follow controls, clients, subscriptions, or delivery events
├── api/v0/SPEC.md                               — 1.x public API contract; read when checking migration behavior
├── remote/v0/SPEC.md                            — 1.x HTTP contract; read when checking migration behavior
└── mcp/2025-11-25/SPEC.md                       — 1.x Model Context Protocol (MCP) contract; read when checking migration behavior
```

## Maintaining the Docs Map

Documentation should help readers use or change the current system. Keep lasting
design decisions with the architecture, exact behavior in specifications, and
performance claims beside useful measurements and runnable benchmarks. Git
history preserves superseded plans and investigation diaries.
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

# Git hygiene
- Keep the repository small, readable, and useful to future contributors.
- Use Git history to preserve source versions. Keep disposable experiment and build output outside the repository.
- Performance evidence should support review and reproduction through runnable benchmarks, concise measurements, and source revision references.
- Each commit should express one coherent change and preserve unrelated work.

# Guidance
- no pip in this project -- uv only
