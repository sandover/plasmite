<!--
Purpose: Design document for replay mode — time-faithful playback of pool messages.
Exports: N/A (documentation).
Role: Design/decision record for maintainers; guides implementation tasks.
Invariants: Replay is "peek with timing"; speed multiplier is the only control in v1.
-->

# Replay Mode Design

## Overview

Replay mode plays back pool messages at the pace they were originally written
(or at a configurable speed multiplier). It is conceptually "peek with timing."

## Primary use cases

- **Game replay**: reconstruct what happened, at the tempo it happened.
- **Capture playback**: touch, speech, gesture, or sensor streams where timing
  is semantically meaningful.

## Command structure

Replay is a new top-level command, not a peek flag. This keeps peek's streaming
semantics clean (peek = as-fast-as-possible) and avoids flag-combination
complexity.

```
pls replay <POOL> [--tail N] [--speed FACTOR] [--since DURATION] [--where EXPR]
```

### `--speed FACTOR`

Floating-point multiplier applied to inter-message delays.

| Value | Meaning |
|-------|---------|
| `1`   | Realtime (default for replay) |
| `2`   | 2× faster |
| `0.5` | Half speed |

Omitting `--speed` implies `--speed 1`.

### Compatibility with existing flags

Replay supports the same filtering and selection flags as peek:

- `--tail N` — select the last N messages to replay.
- `--since DURATION` — select messages from a time window.
- `--where EXPR` — filter messages by jq expression (repeatable, AND).
- `--format pretty|jsonl` — output format (default: pretty).
- `--data-only` — emit only the `.data` payload.
- `--one` — exit after the first matching message.

### Flags NOT supported in v1

- `--timeout` — not meaningful for replay (no indefinite blocking).

## Timing semantics

- Delays are computed from `time` (timestamp) differences between consecutive
  **emitted** messages (i.e., after filtering).
- The delay before the first message is zero.
- Delays are divided by the speed multiplier: `actual_delay = delta / speed`.
- There is **no gap compression**. A 5-minute gap at 1× speed waits 5 minutes.
  This is the contract: faithful reproduction of timing.
- Replay exits when all selected messages have been emitted.
- Replay does not follow live writes. It is a bounded playback of historical
  messages (unlike peek, which streams indefinitely).

## Edge cases

| Scenario | Behavior |
|----------|----------|
| Speed ≤ 0 | Rejected at argument parsing with a usage error. |
| No messages match | Exit 0 immediately, no output. |
| Single message | Emit immediately, exit 0. |
| Messages with identical timestamps | Emit back-to-back, no delay. |
| `--tail 0` (default) | Replay all messages in the pool. |

## Not in v1

These features are explicitly deferred:

- **Pause/resume** — would require interactive terminal control.
- **Seeking** — jump to a specific seq or timestamp.
- **Loop mode** — restart replay when done.
- **Exported file input** — replay from non-pool sources.

## Implementation notes

Replay reads all candidate messages first (like peek's tail path), then emits
them with inter-message sleeps. This is simpler than interleaving reads and
sleeps, and avoids complexity around live writes during replay.

The core loop:

1. Open pool, seek, collect matching messages into a `Vec`.
2. Emit the first message immediately.
3. For each subsequent message, compute `delta = msg[i].time - msg[i-1].time`,
   sleep for `delta / speed`, then emit.
4. Exit 0.
