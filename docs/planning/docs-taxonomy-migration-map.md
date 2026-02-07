<!--
Purpose: Record the file-by-file migration from mixed docs paths to planning vs record taxonomy.
Exports: N/A (documentation).
Role: Migration map and rationale for the docs split.
Invariants: Canonical destinations listed here are the source of truth for post-migration placement.
-->

# Docs Taxonomy Migration Map

This map covers the original `docs/*.md` files that existed before the split.

## Classification defaults

- `record`: stable operational/reference docs intended as current truth
- `planning`: proposals, strategy docs, design notes, spikes, benchmark investigations
- `record (ADR)`: architecture decision records in `docs/decisions/`
- `compatibility stub`: old path kept as pointer to canonical destination

## File-by-file mapping

| Original path | Class | Canonical destination | Rationale |
|---|---|---|---|
| `docs/README.md` | index | `docs/README.md` | Taxonomy entrypoint remains at top-level docs. |
| `docs/architecture.md` | record | `docs/record/architecture.md` | Current architecture reference. |
| `docs/api-quickstart.md` | record | `docs/record/api-quickstart.md` | Stable user-facing quickstart. |
| `docs/doctor.md` | record | `docs/record/doctor.md` | CLI diagnostics reference. |
| `docs/exit-codes.md` | record | `docs/record/exit-codes.md` | Stable scripting contract reference. |
| `docs/go-quickstart.md` | record | `docs/record/go-quickstart.md` | Stable quickstart guide. |
| `docs/homebrew.md` | record | `docs/record/homebrew.md` | Packaging/release operational guidance. |
| `docs/libplasmite.md` | record | `docs/record/libplasmite.md` | C ABI build/link reference. |
| `docs/pattern-matching.md` | record | `docs/record/pattern-matching.md` | Feature guide documenting supported behavior. |
| `docs/releasing.md` | record | `docs/record/releasing.md` | Release process/runbook. |
| `docs/TESTING.md` | record | `docs/record/TESTING.md` | Test process/runbook. |
| `docs/vision.md` | planning | `docs/planning/vision.md` | Living strategy direction, not immutable contract. |
| `docs/roadmap.md` | planning | `docs/planning/roadmap.md` | Time-varying plan, not record truth. |
| `docs/perf.md` | planning | `docs/planning/perf.md` | Benchmark investigations are contextual and evolving. |
| `docs/test-coverage-expansion-proposal.md` | planning | `docs/planning/test-coverage-expansion-proposal.md` | Explicit proposal artifact. |
| `docs/benchmarks/get-scan-baseline-pre-index.md` | planning | `docs/planning/benchmarks/get-scan-baseline-pre-index.md` | Historical benchmark baseline note. |
| `docs/design/color-json.md` | planning | `docs/planning/design/color-json.md` | Design proposal/spike. |
| `docs/design/ingest-review.md` | planning | `docs/planning/design/ingest-review.md` | Post-implementation review note. |
| `docs/design/pattern-matching-spike-prototype.md` | planning | `docs/planning/design/pattern-matching-spike-prototype.md` | Explicit spike output. |
| `docs/design/pattern-matching-ux-research.md` | planning | `docs/planning/design/pattern-matching-ux-research.md` | UX exploration research. |
| `docs/design/pattern-matching-v0-iterative-spec.md` | planning | `docs/planning/design/pattern-matching-v0-iterative-spec.md` | Iterative draft/spec exploration. |
| `docs/design/pool-correctness-refactor.md` | planning | `docs/planning/design/pool-correctness-refactor.md` | Refactor design note. |
| `docs/design/pool-snapshots.md` | planning | `docs/planning/design/pool-snapshots.md` | Snapshot design note. |
| `docs/design/replay-mode.md` | planning | `docs/planning/design/replay-mode.md` | Feature design document. |
| `docs/design/tdd-public-api-v0.md` | planning | `docs/planning/design/tdd-public-api-v0.md` | Public API proposal TDD. |
| `docs/design/tdd-v0.0.1.md` | planning | `docs/planning/design/tdd-v0.0.1.md` | Implementation design doc. |
| `docs/design/web-ui-single-page-v1.md` | planning | `docs/planning/design/web-ui-single-page-v1.md` | Web UI design plan. |
| `docs/decisions/README.md` | record (ADR) | `docs/decisions/README.md` | ADR index is canonical. |
| `docs/decisions/0001-transport-strategy-tcp-now-quic-later.md` | record (ADR) | unchanged | Accepted architecture decision. |
| `docs/decisions/0002-per-entry-checksums-not-in-v0.md` | record (ADR) | unchanged | Accepted architecture decision. |
| `docs/decisions/0003-pattern-matching-interface-deferred.md` | record (ADR) | unchanged | Accepted architecture decision. |
| `docs/decisions/0004-remote-protocol-http-json.md` | record (ADR) | unchanged | Accepted architecture decision. |
| `docs/decisions/0005-inline-seq-index.md` | record (ADR) | unchanged | Accepted architecture decision. |
| `docs/pool-correctness-refactor.md` | compatibility stub | `docs/planning/design/pool-correctness-refactor.md` | Legacy alias retained. |
| `docs/pool-snapshots.md` | compatibility stub | `docs/planning/design/pool-snapshots.md` | Legacy alias retained. |

## Compatibility shims

- Legacy top-level doc paths now contain redirect stubs to canonical `docs/record/` or `docs/planning/` destinations.
- `docs/design/` remains as a compatibility shim directory pointing to `docs/planning/design/`.
