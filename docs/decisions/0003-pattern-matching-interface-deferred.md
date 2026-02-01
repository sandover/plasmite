<!--
Purpose: Record that pattern matching is desired, but the interface is intentionally deferred.
Exports: N/A (documentation).
Role: ADR (Proposed).
Invariants: The public interface should remain stateless; callers pass the range/filter parameters explicitly.
-->

# ADR 0003: Pattern matching queries (interface deferred)

- Date: 2026-02-01
- Status: Proposed

## Context

We want a way to “seek” and filter messages by pattern matching, but we do not want a stateful client-visible next/prev/cursor API surface.

## Decision

Defer the exact CLI interface for pattern matching until we spend time on ergonomics and compatibility constraints.

Constraints to preserve:
- The interface should remain **stateless** (callers supply range + filters each call).
- Start narrow (likely `meta.descrips`) and remain extensible.

