# Vision

Plasmite is the simplest reliable way to coordinate multiple processes with structured messages, without requiring external infrastructure.

## North star

A developer should be able to create, inspect, stream, and automate message workflows using plain files and stable JSON contracts in minutes.

## Target use cases

- Local and host-adjacent process coordination.
- Script-friendly event logging and replay.
- Lightweight service-to-service messaging for small deployments.
- Embedded queue semantics through language bindings.

## Product principles

These are invariants, not preferences. Each states a violation condition.

**Local-first.** Core workflows never require a running daemon, broker, or external service. A developer must be able to create a pool, feed messages, and follow or fetch them with zero configuration beyond the CLI itself. Any feature that makes a network call part of the default path is a violation.

**JSON-first.** Every interface surface — CLI stdout, public API return types, HTTP responses — exposes messages as plain JSON. No interface may require users to understand binary formats to read or write messages. The Lite3 encoding is an internal implementation detail and must never leak to callers.

**Predictable contracts.** CLI flags, API types, HTTP endpoints, and on-disk format versions are stable within a major version. A script written for v0 must run unmodified on any v0.x release. Breaking an existing flag, type, or endpoint requires a major version increment and a documented migration path. Merging a behavior change without a corresponding spec update is a violation.

**Progressive capability.** Remote access and advanced features are always opt-in. The single-host workflow must be fully functional with zero flags beyond pool name. Users must not need to understand remote concepts to do local work.

**Operational clarity.** Every error must be actionable. No error may be emitted without either a concrete resolution hint or a reference to documentation. Vague errors that name the failure without guiding resolution are violations.

## Scope boundaries

In scope:

- Durable bounded pools with concurrent readers/writers.
- CLI + bindings + HTTP interfaces over one shared correctness core.
- Versioned specs and ADRs for decisions that affect compatibility.

Out of scope — these are hard stops, not positioning decisions:

- Cluster-scale message broker features. Plasmite will never add distributed coordination, leader election, or consumer groups.
- Complex schema or type systems beyond the JSON envelope. The envelope (`seq`, `time`, `meta`, `data`) is the full contract; mandatory schema validation or typed fields are violations.
- Hidden server-side state machines. Any feature that requires the server to maintain per-client session state that affects message delivery is out of scope.
- Implicit behavior. Features that change observable output based on undocumented heuristics violate the JSON-first and predictable-contracts principles.

## Success criteria

These are checkable, not aspirational.

- A user with no prior plasmite knowledge can execute a create/feed/follow round-trip using only `plasmite --help` output.
- Any script written against v0 CLI flags and output format runs without modification on any v0.x release.
- New commands or flags do not change the help output or behavior of existing commands as a side effect.
- Pool operations complete without requiring network access when operating on local pools.
- Every error returned by the CLI includes either a `hint` field or a documentation reference.

## Strategy implications

- Keep core storage and validation logic centralized and reusable across all interface surfaces.
- Prefer explicit flags and observable behavior over implicit heuristics and magic defaults.
- Treat docs of record as product surface: architecture, vision, and specs must stay coherent with each other and with the running code.
- When a feature request conflicts with the principles above, the principles win. Features are added; principles are not compromised for convenience.
