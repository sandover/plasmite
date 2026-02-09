# ADR 0003: Pattern matching queries (interface deferred)

- Date: 2026-02-01
- Status: Proposed

## Context

We want a way to “seek” and filter messages by pattern matching, but we do not want a stateful client-visible next/prev/cursor API surface.

## Decision

Defer the exact CLI interface for pattern matching until we spend time on ergonomics and compatibility constraints.

Constraints to preserve:
- The interface should remain **stateless** (callers supply range + filters each call).
- Start narrow (likely `meta.tags`) and remain extensible.

