# Ingest review (post-implementation)

## Summary
Focused cleanup pass to reduce duplication and keep the ingestion pipeline small and explicit.

## Changes
- Extracted JSON-seq record handling into a single helper to avoid duplicated parse/oversize paths.

## Rationale
Keeping each mode’s control flow clear while sharing small, localized helpers prevents subtle
divergence across record-boundary parsers without introducing heavyweight abstractions.
