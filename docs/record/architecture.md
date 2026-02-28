# Architecture

This is the implementation architecture for Plasmite.
Normative behavior lives in:

- `spec/v0/SPEC.md` (CLI contract)
- `spec/api/v0/SPEC.md` (public API)
- `spec/remote/v0/SPEC.md` (HTTP protocol)

## Design principles

These are operational constraints, not labels. Each states a violation condition.

**Local-first.** The core layer must compile and pass all tests with no network dependencies. Pool operations must never make network calls. Any dependency introduced into `src/core` that requires network access is a violation.

**Contract-first.** Public API types, CLI flag shapes, and HTTP endpoint paths are frozen within a major version. Internal refactors that preserve observable behavior are always allowed. Changes that alter observable behavior require a spec update before the code change. Merging a behavior change without a corresponding spec update is a violation.

**Shared core.** CLI, API, and HTTP server must all invoke pool and message operations through `src/core` or `src/api`. Duplicating correctness logic across interface adapters is a violation. Shared interface helpers (`pool_paths.rs`, `pool_info_json.rs`) exist precisely to prevent parallel implementations; adding a parallel implementation is a violation.

**Explicit failure modes.** Every error returned by a public interface must map to a stable `ErrorKind`. Adding a new failure case requires adding or reusing an existing `ErrorKind`. Returning a generic internal error for a condition that has a more specific kind is a violation.

## Layer model

```
Interface layer  →  Core domain layer  →  Platform layer
```

**Interface layer** (`src/main.rs`, `src/command_dispatch.rs`, `src/api/*`, `src/serve.rs`, `src/abi.rs`):
- Must: parse and validate inputs, invoke core operations, map results to user-facing formats.
- Must not: duplicate storage correctness logic across multiple interface surfaces.
- Allowed exception: one shared validation-report implementation may perform read-only ring/frame checks when exposed through API/CLI diagnostics.
- Must not: hold pool state across requests beyond what `Pool` and `Cursor` already encapsulate.

**Core domain layer** (`src/core/*`):
- Must: own all append/get/tail semantics, validation, sequencing, and corruption detection.
- Must not: call into interface-layer modules.
- Must not: make network calls or depend on a runtime async context.

**Platform layer** (`memmap2`, `fs2`, `libc`):
- Must: provide concurrency and durability primitives.
- Must not: contain message-semantic logic.

**Cross-layer rule:** Interface layers must not fork storage correctness logic into per-surface implementations. A single shared validator used by diagnostics is acceptable; embedding bespoke ring/frame/seq rules inside CLI handlers, HTTP handlers, and ABI wrappers independently is a violation.

## Data model and on-disk layout

A pool file is:

`header | index_region | ring`

- Header: metadata, bounds, and offsets.
- Index region: optional fixed-size seq→offset slots (`(u64 seq, u64 offset)`).
- Ring: append log frames containing encoded `{meta, data}` payloads.

Key invariants:

- Sequence numbers are monotonically increasing for committed messages.
- Frame commit state is validated before exposure to readers.
- Corrupt, torn, or stale reads do not silently return invalid payloads.
- Index mismatches always fall back to scan for correctness.
- A binary must refuse to open a pool with a format version it does not understand; an unknown version produces an actionable error, not a panic or silent data access.
- Format version increments are additive within a major version where possible; breaking changes to the on-disk layout require a new format version.

## Write/read paths

Append (high level):

1. Validate input and prepare frame.
2. Acquire writer lock.
3. Plan placement in ring (including overwrite/wrap decisions).
4. Commit frame bytes.
5. Update index slot (when enabled).
6. Publish header updates and notify waiters.

Invariants:

- The writer lock must be held for the full duration of steps 3–6. Releasing it between planning and committing is a violation.
- A frame in `Writing` state that is never committed must not be returned to readers.
- `plan.rs` must remain pure and side-effect-free. It must not write to the pool file. Its output must be fully determined by its inputs.

Get-by-seq path:

1. Validate requested seq is in visible bounds.
2. Probe index slot.
3. If probe fails validation, fall back to scan.
4. Return decoded message envelope.

Tail path:

1. Resolve start position (`tail`, `since`, `from`).
2. Stream committed messages in order.
3. Use notify + bounded polling fallback for low-latency follow mode.

Invariant: Correctness of the tail path must not depend on notify delivery. Notify is a latency optimization; a tail that never receives a notification must still eventually return all committed messages. Removing notify must not cause failures in non-timing tests.

## Transport architecture

Plasmite is transport-agnostic at the core.

- Local mode: CLI/API calls directly into core operations.
- Remote mode: `plasmite serve` adapts HTTP request/response into the same core calls.
- Process capture mode (`plasmite tap`): CLI spawns a child process, reads stdout/stderr on separate threads, and appends line messages through the same local append path as `feed`.
- Future transports must be adapters over the existing core, not alternate correctness engines.

Invariant: Adding a new transport must not require changes to `src/core`. If a proposed transport requires core changes to function correctly, the design is wrong.

## Tap execution model (CLI adapter)

`tap` is an interface-layer adapter over existing local pool operations:

1. Parse tap arguments and resolve a local pool ref.
2. Spawn child process with inherited stdin and piped stdout/stderr.
3. Run one reader thread per stream (`stdout`, `stderr`) and frame each line as a JSON message.
4. Emit lifecycle messages (`start`, then `exit`) around captured output.
5. Append all messages via the shared local append path; tap does not introduce a parallel storage path.
6. On Unix, forward SIGINT/SIGTERM received by tap to the child, then drain buffered output before emitting the exit lifecycle message.

Invariant: tap may add ingestion behavior, but it must not fork correctness semantics from the shared append/read contracts.

## Extension seams

- New interfaces must reuse `PoolRef` and core message operations. Parallel pool-resolution logic is a violation.
- New payload conventions must preserve existing envelope semantics (`seq`, `time`, `meta`, `data`). Removing or renaming envelope fields is a breaking change.
- Performance work must preserve crash-safety and fallback correctness paths. Optimizations that bypass the index fallback-to-scan path are violations.
- New commands or flags must not change the observable behavior of existing commands as a side effect.

## Quality criteria

These are auditable claims about the implementation. Each is checkable by inspection or test.

- `src/core` contains no `async` code and no network dependencies.
- All public error conditions map to a named `ErrorKind` variant; no public path returns a generic internal error for a condition with a more specific kind.
- All three interface surfaces (CLI, API, HTTP) exercise the same underlying pool operations for equivalent actions.
- `plan.rs` has no side effects; its output is fully determined by its inputs.
- The notify path is never on the correctness critical path; removing notify must not cause failures in non-timing tests.
- Pool files opened with an unknown format version produce an actionable error, not a panic or silent data access.
- The C ABI (`abi.rs`) remains adapter-focused: parameter translation, error mapping, and session/poll orchestration only; it must not introduce separate storage correctness semantics from `src/core`/`src/api`.

## Operational guarantees vs non-goals

Guaranteed:

- Stable error-kind surface and exit-code mapping.
- Durable pool file invariants under normal crash and restart scenarios.
- Cross-surface behavior parity (CLI/API/HTTP) for the same operation class.

Not guaranteed:

- Hard real-time latency.
- Infinite retention (ring buffers are bounded).
- Backward compatibility for undocumented internal file details outside versioned formats.
