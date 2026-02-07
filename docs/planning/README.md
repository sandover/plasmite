<!--
Purpose: Define the single planning namespace and keep active planning artifacts minimal.
Exports: N/A (documentation).
Role: Planning policy and index.
Invariants: Planning docs are temporary and non-authoritative until promoted to docs-of-record.
-->

# Planning

`docs/planning/` is the only planning namespace.

Use it only for active, in-flight planning artifacts.
Stale plans, spikes, and superseded drafts should be deleted.

## Active planning docs

- Roadmap: `docs/planning/roadmap.md`

Promotion workflow:

1. Draft/update in `docs/planning/`.
2. Review and accept.
3. Promote accepted content to `docs/record/` (or `docs/decisions/` for ADRs).
