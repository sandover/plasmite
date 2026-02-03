# Pool resize strategy (design note)

## Goal
Define safe, minimal resize semantics that avoid mmap hazards while keeping reader behavior predictable.

## Constraints
- Readers may have active mmaps; shrinking a file can trigger SIGBUS on read.
- Writers must preserve ring invariants (monotonic seq, bounds correctness).
- v0.0.1 favors correctness over online flexibility.

## Recommended policy
1. **No live resize with attached readers/writers** in v0.0.x.
   - Resize requires exclusive access (no other processes attached).
2. **Grow is allowed offline**, shrink is **offline-only** and should be conservative.
3. **Shrink is two-phase**:
   - Logical shrink: update header to a smaller ring size (future readers honor it).
   - Physical truncate: only when no other process can have the file mmapped.
4. Prefer **copy-to-new-file** for shrink:
   - Create a new pool at target size and replay/export selected messages.
   - Atomically swap (rename) when done.

## Reader semantics
- Readers should never assume stable mmap size across process lifetime.
- Any resize tool should require that no readers are attached (explicit lock or out-of-band coordination).

## Rationale
This mirrors safe behavior in mmap-based systems: avoid SIGBUS, keep invariants simple,
and defer complex live-resize mechanisms until a later release with explicit coordination.

## Deferred ideas
- Live grow via remap and header versioning.
- Monotonic virtual offsets or reader-side remap coordination.
