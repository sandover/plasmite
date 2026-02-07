<!--
Purpose: Explain what Plasmite is, who it’s for, and what it intentionally is not.
Exports: N/A (documentation).
Role: North-star guidance; used to judge new features and avoid scope creep.
Invariants: Local-first and JSON-first; the CLI remains script-stable.
-->

# Vision

Plasmite is a **local-first, JSON-first message pool** for “a handful to dozens” of cooperating processes on a machine (and eventually a small LAN).

## What it is

- A CLI, library, and HTTP server for creating pools, appending JSON messages, and observing messages as they arrive.
- Language bindings (Go, Python, Node.js) for embedding without subprocess overhead.
- A durable-ish local IPC/log primitive with a stable, script-friendly contract.
- Remote access via HTTP (`plasmite serve`) for multi-machine coordination.

## What it is not (non-goals)

- A recreation of legacy Plasma’s full slaw/protein type system.
- A stateful client-visible cursor protocol (callers can always be explicit about ranges).

