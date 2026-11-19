# Vision

Plasmite is the simplest reliable way for programs and AI agents to coordinate with structured messages, without external infrastructure.

## North star

A developer or an AI agent can create, inspect, stream and automate message workflows in minutes, using plain files, JSON and the tools they already know. One small model serves them whether they arrive through the shell, a library or a Model Context Protocol (MCP) client, and it stays the same as their work grows.

## The model

A pool is a message channel that outlasts the programs and agents using it. It's a ring buffer on disk that keeps the most recent messages, up to a fixed size, and drops the oldest when it fills. Writers and readers come and go independently.

Three verbs cover the work: **feed** puts a message into a pool, **follow** waits for new messages, and **fetch** reads messages the pool already holds. Every read starts at a position and moves forward.

Every message has the same four fields: `seq`, `time`, `meta` and `data`. The pool assigns `seq`, which identifies the message and defines its order. `time` records what the clock read when the pool stored the message. `meta` holds Plasmite's metadata, such as tags for filtering, and `data` is a JSON object the application chooses.

## Target use cases

- Coordinating processes on one host or across nearby hosts.
- Coordinating AI agents and the people supervising them: handoffs, progress and shared history that survive restarts and context resets, across harnesses and machines. An agent can join through MCP with nothing installed.
- Logging events from scripts and re-reading the history later.
- Capturing any command's output with a pipe into `feed`, then reading it back with `follow` and `fetch`.
- Lightweight messaging between services in small deployments.
- Embedded queues inside applications through the language libraries.

## Principles

These are invariants, not preferences. Each states what would violate it.

**Local-first.** Core workflows never need a running daemon, broker or external service. A developer can create a pool, feed messages, and follow or fetch them with no configuration beyond the CLI itself. Any feature that puts a network call on the default path is a violation.

**One model.** Every interface offers the same verbs and options under the same names, spelled in each language's style. An operation or option that exists in one interface with no counterpart in the others is a violation. The CLI is one more caller of the library: its text handling, warnings, exit codes and shorthand belong to it as a program and add no new concepts.

**Small pieces, loosely joined.** Plasmite offers options where the pool can do the work better than the caller, such as finding a position, checking tags without unpacking messages, and flushing its file to disk. The caller chooses when to stop, when to retry and how to recover from loss. CLI commands compose with shell tools. When the OS reports a closed output pipe, `follow` exits. `follow --one` also works in shells that keep the pipe open after taking one message.

**`seq` is the truth; `time` is an observation.** The pool assigns `seq`, and it alone defines identity and order. `time` records what the clock said, and Plasmite never adjusts it. A guarantee that depends on clock behavior is a violation.

**No silent loss.** A local or HTTP reader whose position falls behind retention reports the missing sequence range and where to continue. A UDP reader reports any skip or retention error it receives from the server. The network can drop these notices or the messages themselves.

**JSON-first.** Every interface provides a stable plain-JSON path for reading and writing messages. CLI commands print readable output in a terminal and compact JSON in a pipe or file, and a global format choice overrides that rule. Binary formats are optional paths for speed. No normal message workflow may require understanding a binary format.

**Predictable contracts.** CLI flags, library types, HTTP endpoints and on-disk formats stay stable within a major version. A script written for one major version runs unmodified across that major version. Any break to an existing flag, type or endpoint requires a new major version, and that release states whether it provides migration guidance. A behavior change merged without a matching spec update is a violation.

**Progressive capability.** Remote access and advanced features are always opt-in. The single-host workflow works fully with nothing beyond a pool name, and local work never requires understanding remote concepts. An explicit HTTPS pool address (`https://host:port/<pool>`) is the opt-in path to remote pools from core commands, and loopback HTTP serves credential-free use on the server machine.

**Operational clarity.** Errors explain what failed and name the affected resource when known. They add recovery guidance when it helps the user act; a clear error needs no extra hint.

## Scope

In scope:

- Durable bounded pools with concurrent readers and writers.
- CLI, language libraries, HTTP and optional UDP interfaces over one shared correctness core.
- Documented stream subscriptions with bounded, temporary state for selection, routing, leases, cancellation and finite completion.
- Versioned specs and docs of record for every decision that affects compatibility.

Out of scope. These are hard stops, not positioning:

- Cluster-scale broker features. Plasmite never adds distributed coordination, leader election or consumer groups.
- Search and query languages. A pool has a fixed size, so a reader can scan all of it, and each language already filters well. Tags are the only built-in filter.
- Schema or type systems beyond the message fields. `seq`, `time`, `meta` and `data` are the full contract; mandatory schema validation or typed fields are violations.
- Durable reader progress on the server. Plasmite doesn't store per-reader positions, track acknowledgements, retry deliveries or choose a reader's recovery policy. Readers own those decisions. A stream may keep bounded, temporary state to send selected messages and reclaim resources when the reader closes or disappears. That state expires with the subscription, never changes pool contents, and never stores reader progress.
- Transport rules that discard valid received messages to impose a delivery guarantee nobody asked for. A UDP follow delivers matching messages in arrival order, including late messages and duplicates; readers choose their own ordering policy.
- Implicit behavior. A feature that changes observable output based on undocumented heuristics violates the JSON-first and predictable-contracts principles.

## Success criteria

These are checkable, not aspirational.

- A user with no prior Plasmite knowledge can create a pool, feed a message and follow it using only `plasmite --help`.
- One shared set of conformance scenarios passes in every language library and the CLI.
- Any script written against a CLI major version runs unmodified across that major version.
- Readers and writers share one message shape and matching encodings, and a pipe from `follow` into `feed` preserves `data` and `tags`.
- Pool operations on local pools complete without network access.
- A user can capture any command's output and read it back using only `feed` and `follow`, with no changes to the command.
- CLI errors identify the failure and the affected resource, with recovery guidance when known.

## Strategy

- Keep storage and validation logic in one core that every interface reuses.
- Keep the shared rules few and documented: one message shape, positions for every read, and readable output in a terminal with JSON in a pipe. The CLI adds its own text rules, such as telling whole messages from plain data by their shape. Add an explicit flag only when intent or encoding can't follow those rules.
- Treat docs of record as product surface. Vision, architecture and specs stay coherent with each other and with the running code.
- When a feature request conflicts with these principles, the principles win.
