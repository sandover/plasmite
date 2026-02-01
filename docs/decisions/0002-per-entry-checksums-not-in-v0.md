<!--
Purpose: Capture the integrity motivation and scheduling for per-entry checksums.
Exports: N/A (documentation).
Role: ADR (Accepted).
Invariants: Checksums provide integrity detection, not authenticity; enabling them must not weaken correctness guarantees.
-->

# ADR 0002: Per-entry checksums (not in v0.0.1)

- Date: 2026-02-01
- Status: Accepted

## Context

Some users need stronger confidence that bytes read from a pool match what was written, and want corruption to be detected early and reported precisely.

## Decision

- Add per-entry checksums as an **opt-in pool capability** (e.g. `pool create --checksum`).
- Do **not** ship this in v0.0.1.
- When enabled:
  - Writes record a checksum per entry.
  - Reads verify checksums and fail with a specific “invalid/corrupt pool” error on mismatch.

## User story

“My job is important; I prefer a clear integrity failure over silently producing incorrect output.”

This targets bitrot, partial writes, disk/controller bugs, or unintended modification.

