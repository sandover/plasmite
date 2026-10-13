# Vision

Plasmite is the simplest reliable way to coordinate multiple processes with structured messages, without requiring external infrastructure.

## North star

A developer should be able to create, inspect, stream, and automate message workflows using plain files and stable JSON contracts in minutes.

## Target use cases

- Local and host-adjacent process coordination.
- Script-friendly event logging and replay.
- Zero-integration process output capture via `plasmite tap`, then filter/replay with `follow`.
- Lightweight service-to-service messaging for small deployments.
- Embedded queue semantics through language bindings.

## Product principles

These are invariants, not preferences. Each states a violation condition.

**Local-first.** Core workflows never require a running daemon, broker, or external service. A developer must be able to create a pool, feed messages, and follow or fetch them with zero configuration beyond the CLI itself. Any feature that makes a network call part of the default path is a violation.

**JSON-first.** Every interface surface provides a stable plain-JSON path for reading and writing messages. CLI commands show readable output in terminals and compact JSON in pipes or files; a global format choice overrides that documented rule. HTTP may expose explicit Lite3 byte endpoints as additive performance paths, but JSON endpoints remain canonical. No interface may require users to understand binary formats to complete normal message workflows.

**Predictable contracts.** CLI flags, API types, HTTP endpoints, and on-disk format versions are stable within a major version. A CLI script written for one package major version must run unmodified within that major version. The approved 1.2.0 release is an explicit breaking cutover exception: it adopts complete messages and retires the old flags without compatibility modes. Other breaks to an existing flag, type, or endpoint require a major version increment. Each breaking release must state whether it provides migration guidance or retires the old interface without a migration path. Merging a behavior change without a corresponding spec update is a violation.

**Progressive capability.** Remote access and advanced features are always opt-in. The single-host workflow must be fully functional with zero flags beyond pool name. Users must not need to understand remote concepts to do local work. Explicit HTTPS pool refs (`https://host:port/<pool>`) are the opt-in path for remote mode from core commands; loopback HTTP serves credential-free use on the server machine.

**Operational clarity.** Every error must be actionable. No error may be emitted without either a concrete resolution hint or a reference to documentation. Vague errors that name the failure without guiding resolution are violations.

## Scope boundaries

In scope:

- Durable bounded pools with concurrent readers/writers.
- CLI + bindings + HTTP interfaces over one shared correctness core.
- Versioned specs and canonical docs of record for decisions that affect compatibility.

Out of scope — these are hard stops, not positioning decisions:

- Cluster-scale message broker features. Plasmite will never add distributed coordination, leader election, or consumer groups.
- Complex schema or type systems beyond the JSON envelope. The envelope (`seq`, `time`, `meta`, `data`) is the full contract; mandatory schema validation or typed fields are violations.
- Hidden server-side state machines. Any feature that requires the server to maintain per-client session state that affects message delivery is out of scope.
- Implicit behavior. Features that change observable output based on undocumented heuristics violate the JSON-first and predictable-contracts principles.

## Success criteria

These are checkable, not aspirational.

- A user with no prior plasmite knowledge can execute a create/feed/follow round-trip using only `plasmite --help` output.
- Any script written against the current CLI major version runs without modification within that major version.
- Message readers and writers share one complete-message shape and matching encodings; default read/write pipes preserve data and tags.
- Pool operations complete without requiring network access when operating on local pools.
- Every error returned by the CLI includes either a `hint` field or a documentation reference.
- A user can capture and replay any command's stdout/stderr using only `tap` and `follow`, with no changes to the wrapped program.

## Strategy implications

- Keep core storage and validation logic centralized and reusable across all interface surfaces.
- Use a few documented shared rules: complete messages, syntax-based input detection, and terminal versus pipe output. Use explicit flags only when intent or encoding cannot follow those rules.
- Treat docs of record as product surface: architecture, vision, and specs must stay coherent with each other and with the running code.
- When a feature request conflicts with the principles above, the principles win. Features are added; principles are not compromised for convenience.
