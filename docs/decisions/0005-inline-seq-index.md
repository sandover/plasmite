# ADR 0005: Inline seq->offset index in pool files

- Status: Accepted
- Date: 2026-02-06
- Supersedes: deferred persistent TOC notes captured during early v0 planning
- Related trail: historical planning epics `HT2QNJ` and `R5PFIA` discussed TOC/perf options

## Context

Pre-index, `get(seq)` depended on scan behavior. For larger pools this made random reads noticeably slower and exposed scan-position variance (`near_newest`/`mid`/`near_oldest`).

We needed a solution that:
- Improves point lookup latency for all entry points (CLI, API, HTTP, bindings).
- Keeps correctness local to the storage format (not per-process caches).
- Preserves crash-safety and simple writer ordering.

## Decision

Use a fixed-size inline index region in each pool file:
- Layout: `header | index_region | ring`.
- Slot format: `(seq: u64, offset: u64)` (16 bytes).
- Slot selection: `seq % index_capacity`.
- Default sizing: auto-sized from pool size; can be overridden, including `index_capacity=0`.

Read/write behavior:
- Append order: commit frame -> write index slot -> publish header.
- `get(seq)` first probes index slot; on any mismatch/staleness/torn-read suspicion, it falls back to scan.
- Index collisions overwrite older slot contents by design; fallback preserves correctness.

## Rationale

Why not persistent TOC:
- A TOC ring plus head/tail management adds format and update complexity without clear value over direct modulo addressing for v0 needs.
- Inline modulo slots deliver O(1) recent lookups with simpler invariants.

Why not in-memory-only cache:
- In-memory caches only help long-lived processes (for example a server), but do not help CLI invocations or separate binding processes.
- On-disk index gives a shared optimization that works uniformly.

Crash-safety notes:
- Crash after frame commit but before index write: index stale, scan fallback still correct.
- Crash after index write but before header publish: new seq not yet visible via header bounds; next writes reconcile state.
- Torn/stale slot reads are contained by frame seq validation before returning data.

## Consequences

- On-disk format bumped to v3.
- Pools now include `index_capacity` and `index_offset` metadata.
- Small append overhead (index slot write; optional flush in `durability=flush` path).
- Documented fallback remains essential for overwritten/stale index entries.

SeqOffsetCache remains in code as an optional future L1 optimization path, but inline index is now the primary v0 random-access mechanism.
