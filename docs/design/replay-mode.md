<!--
Purpose: Design document for replay mode — time-faithful playback of pool messages.
Exports: N/A (documentation).
Role: Design/decision record for maintainers; guides implementation tasks.
Invariants: Replay is a peek flag (--replay N); speed multiplier is the only control in v1.
-->

# Replay Mode Design

## Overview

Replay mode plays back pool messages at the pace they were originally written
(or at a configurable speed multiplier). It is implemented as `--replay N` on
the existing `peek` command, not as a separate command.

## Primary use cases

- **Game replay**: reconstruct what happened, at the tempo it happened.
- **Capture playback**: touch, speech, gesture, or sensor streams where timing
  is semantically meaningful.

## Command structure

Replay is a flag on peek: `--replay <SPEED>`.

```
pls peek <POOL> --tail N --replay <SPEED> [--since DURATION] [--where EXPR]
```

### `--replay <SPEED>`

Floating-point multiplier applied to inter-message delays.

| Value | Meaning |
|-------|---------|
| `0`   | No delay (bounded dump, exits when done) |
| `1`   | Realtime |
| `2`   | 2× faster |
| `0.5` | Half speed |

### Design rationale: flag vs. command

Replay is "peek with timing." A separate command would duplicate peek's entire
flag surface (--tail, --since, --where, --format, --data-only, --one) for no
real gain. As a flag, all existing peek options just work.

### Constraint: requires `--tail` or `--since`

`--replay` without `--tail` or `--since` is rejected. Without either, peek
starts at the write head and follows live — there are no historical messages to
apply timing to.

### Compatibility with existing flags

All peek filtering and selection flags work with replay:

- `--tail N` — replay the last N messages.
- `--since DURATION` — replay messages from a time window.
- `--where EXPR` — filter messages by jq expression (repeatable, AND).
- `--format pretty|jsonl` — output format (default: pretty).
- `--data-only` — emit only the `.data` payload.
- `--one` — exit after the first matching message.

## Timing semantics

- Delays are computed from `time` (timestamp) differences between consecutive
  **emitted** messages (i.e., after filtering).
- The delay before the first message is zero.
- When speed > 0: `actual_delay = delta / speed`.
- When speed = 0: no delays (bounded dump).
- There is **no gap compression**. A 5-minute gap at 1× speed waits 5 minutes.
  This is the contract: faithful reproduction of timing.
- Replay exits when all selected messages have been emitted.
- Replay does not follow live writes. It is a bounded playback of historical
  messages (unlike regular peek, which streams indefinitely).

## Edge cases

| Scenario | Behavior |
|----------|----------|
| Speed < 0 | Rejected at argument parsing with a usage error. |
| Speed = 0 | Bounded dump, no delays, exits when done. |
| No `--tail` or `--since` | Rejected with a usage error. |
| No messages match | Exit 0 immediately, no output. |
| Single message | Emit immediately, exit 0. |
| Messages with identical timestamps | Emit back-to-back, no delay. |

## Not in v1

These features are explicitly deferred:

- **Pause/resume** — would require interactive terminal control.
- **Seeking** — jump to a specific seq or timestamp.
- **Loop mode** — restart replay when done.
- **Exported file input** — replay from non-pool sources.

## Implementation notes

When `--replay` is set, `peek` enters a separate code path (`peek_replay`)
that collects all candidate messages first, then emits them with inter-message
sleeps. This avoids interleaving reads and sleeps and sidesteps complexity
around live writes during replay.
