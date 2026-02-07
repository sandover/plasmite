<!--
Purpose: Define the planning namespace for proposals, designs, and in-flight strategy docs.
Exports: N/A (documentation).
Role: Planning-doc index and lifecycle policy.
Invariants: Planning docs are non-authoritative until promoted to docs of record.
-->

# Planning and Design

Place in-progress artifacts here:

- proposals and spikes
- design drafts and technical design docs
- roadmap and strategy notes
- benchmark investigations and temporary analysis

Promotion workflow:

1. Draft/update in `docs/planning/`.
2. Review and accept.
3. Promote the accepted content into `docs/record/` (or `docs/decisions/` for ADRs).
4. Leave a compatibility stub if old links are already in use.
