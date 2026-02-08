<!--
Purpose: Define exact-command rendering policy for missing-pool remediation hints.
Exports: One canonical rendering approach with examples and fallback rules.
Role: Decision artifact for implementing poke/peek missing-pool hints consistently.
Invariants: Hint rendering must not claim exactness when safe reconstruction is impossible.
Invariants: Policy is shell-agnostic and avoids shell-specific escaping assumptions.
Notes: This policy unblocks implementation and test-case authoring for remediation UX.
-->

# Missing-Pool Remediation Exact Command Policy

## Decision

Use a **shell-agnostic argv echo policy** for missing-pool remediation hints.

- Canonical approach: render an exact command only when the CLI already has a stable,
  unambiguous argv token sequence available at error time.
- Non-goal: reconstruct full shell pipeline context or shell-specific quoting semantics.
- Why this approach:
  - avoids bash/zsh-specific escaping that breaks in fish/PowerShell,
  - keeps hints reproducible from argv-level data,
  - keeps integration tests deterministic across developer environments.

Tradeoff versus POSIX-only copy/paste strings:

- POSIX-only formatting can look more polished for bash/zsh users, but is brittle for
  non-POSIX shells and for stdin/pipeline provenance.
- Shell-agnostic wording is slightly less ergonomic but more correct across platforms.

## Rendering Rules

- When safe:
  - render exact command text with explicit argv values and include `--create` suggestion.
- When ambiguous/unsafe:
  - do not claim exact command; use fallback wording that explains what to add/change.
- Never infer data not present in argv context (for example stdin payload body or pipe source).

Required argument-shape coverage:

- inline JSON payloads,
- paths with spaces,
- repeated flags,
- stdin/pipe-driven invocations.

## Fallback Wording

If exact reconstruction is unsafe, use wording like:

- `Pool is missing. Re-run with --create (local refs only).`
- `Could not reconstruct exact command safely (stdin/pipe context); add --create to the same invocation.`

## Examples (Before/After)

1. Inline JSON payload
- Before: `Create it first: plasmite pool create chat`
- After: `Retry with exact command: plasmite poke chat --create '{"x":1}'`

2. Path with spaces
- Before: `Create it first: plasmite pool create my pool`
- After: `Retry with exact command: plasmite poke "my pool" --create --file "data/events 1.json"`

3. Repeated flags
- Before: `Create it first: plasmite pool create incidents`
- After: `Retry with exact command: plasmite poke incidents --create --tag sev1 --tag billing '{"msg":"timeout"}'`

4. Stdin/pipe usage
- Before: `Create it first: plasmite pool create events`
- After: `Pool missing. Add --create to your poke invocation (exact command omitted for stdin/pipe safety).`

## Test Extraction Notes

The four examples above should map directly to CLI integration tests:

- exact command emitted for inline payload,
- exact command emitted with quoted path args,
- exact command preserves repeated flags order,
- fallback wording used when stdin/pipe command cannot be reconstructed safely.
