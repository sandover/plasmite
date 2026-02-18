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

- Local-first by default: no daemon required for core workflows.
- JSON-first UX: every interface is understandable and scriptable.
- Predictable contracts: stable CLI/API/protocol surfaces.
- Progressive capability: start single-host, then opt into remote access.
- Operational clarity: errors and diagnostics should be immediately actionable.

## Scope boundaries

In scope:

- Durable bounded pools with concurrent readers/writers.
- CLI + bindings + HTTP interfaces over one shared correctness core.
- Versioned specs and ADRs for decisions that affect compatibility.

Out of scope:

- Large-cluster message broker positioning.
- Complex schema/type systems beyond JSON envelope semantics.
- Hidden server-side state machines that weaken script predictability.

## Success criteria

- New users can complete create/feed/follow/fetch flows quickly from CLI docs.
- Integrators can rely on stable contracts and clear upgrade guidance.
- Feature additions improve power without increasing conceptual overhead.

## Strategy implications

- Keep core storage + validation logic centralized and reusable.
- Prefer explicit flags and observable behavior over implicit heuristics.
- Treat docs of record as product surface: architecture + vision + specs must stay coherent.
