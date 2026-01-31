# Pool correctness refactor (design note)

Purpose
- Define a refactor plan that separates pure state transitions from IO, and make
  invariants + crash behavior explicit before implementation.

Proposed module boundaries
- core/pool_state.rs
  - Pure data types for header/ring state (no IO). Examples: PoolState, Tail,
    Head, Seq, RingBounds, FrameMeta.
- core/plan.rs
  - Pure planners for mutations: append, drop_oldest, wrap. Produces an
    AppendPlan with explicit write steps and next PoolState.
- core/validate.rs
  - Pure validation: fast tail-only checks and full scans. No IO.
- core/pool_io.rs (or pool.rs apply section)
  - IO application of plans (write frame, commit marker, header update),
    plus error mapping.

Core invariants (selected)
- Offsets:
  - head and tail offsets are within ring bounds and aligned to frame header.
  - head points to newest committed frame start; tail points to oldest committed
    frame start.
- Sequence:
  - seq is strictly increasing by 1 across committed frames.
  - header oldest/newest match the tail/head frames in ring.
- Frame layout:
  - Each committed frame has valid magic, size bounds, and a commit marker.
  - Wrap markers are treated as frame terminators and never returned as data.
- Header:
  - header size and ring size are valid; head offset is derivable from newest
    frame metadata if a full scan is done.

State transition API (pure)
- fn plan_append(state: PoolState, payload_len: u32) -> Result<AppendPlan>
- fn plan_drop_oldest(state: PoolState, bytes_needed: u32) -> Result<DropPlan>
- fn apply_plan(state: PoolState, plan: AppendPlan) -> PoolState
  - apply_plan is pure: returns the next state computed from the plan.

IO application order (append)
1) If wrapping is needed, write wrap marker at end of ring.
2) Write frame header + payload.
3) Write commit marker for the frame (atomic signal of commit).
4) Update header (newest seq, head offset, possibly oldest seq if empty).
5) Persist ordering as needed (fsync/flush decisions stay in IO layer).

Decision: dual-header + checksum scheme
- Not now.
- Rationale: increases complexity and format surface area while we still have
  open questions about append order and validation. Keep task XB3DJ6 as optional
  follow-up once plan/validator/apply are stable and tested.

Crash consistency ordering contract
- If a crash occurs before the commit marker, the frame is treated as
  uncommitted and ignored by readers/validators.
- If a crash occurs after the commit marker but before header update, readers
  may see a valid frame beyond header newest; this is ignored by default and
  can be surfaced by full validation only.
- Header is the source of truth for visible data; ring data beyond newest is
  considered garbage unless revalidated.

Read-side behavior on partial/invalid frames
- Readers scan from tail forward using header bounds.
- On encountering an invalid or partial frame:
  - Stop at that point for normal reads (data beyond is treated as unavailable).
  - Full validator reports corruption details and may recommend rebuild.

Debug validator policy
- Debug builds use full scan after append/drop (debug_assert).
- Release builds use tail-only fast checks on hot paths; full scan only on
  explicit validation paths or tests.

Failure-mode table
| Failure point                              | Observed state                          | Expected behavior |
|--------------------------------------------|-----------------------------------------|-------------------|
| Crash during payload write                 | Partial frame, no commit marker         | Ignore frame      |
| Crash after commit marker, before header   | Frame committed, header newest old      | Header wins       |
| Crash during header write                  | Header may be partially updated         | Header validation fails, report error |
| Crash during wrap marker write             | Partial wrap marker                     | Treat as invalid, stop scan |
| Power loss during fsync                    | Header/ring out of sync                 | Read with header; validator reports |

Crash test phases
- AfterWrap: wrap marker written (if any), frame not started; header unchanged.
- AfterWrite: frame header in Writing state + payload bytes written; header unchanged.
- AfterCommit: frame header updated to Committed; header unchanged.
- AfterHeader: pool header updated to new head/tail/seqs (fully visible).
