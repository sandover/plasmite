<!--
Purpose: Explain what Plasmite is, who it’s for, and what it intentionally is not.
Exports: N/A (documentation).
Role: North-star guidance; used to judge new features and avoid scope creep.
Invariants: Local-first and JSON-first; the CLI remains script-stable.
-->

# Vision

Plasmite is a **local-first, JSON-first message pool** for “a handful to dozens” of cooperating processes on a machine (and eventually a small LAN).

## What it is

- A small CLI that lets you create pools, append JSON messages, and observe messages as they arrive.
- A durable-ish local IPC/log primitive with a stable, script-friendly contract.
- A foundation for future remote access that doesn’t fork the contract surface.

## What it is not (non-goals)

- A recreation of legacy Plasma’s full slaw/protein type system.
- A stateful client-visible cursor protocol (callers can always be explicit about ranges).

