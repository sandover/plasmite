# Plasmite Technical Design Document (Implementation) v0.0.1

This TDD describes the **storage + concurrency + recovery** design for a Plasma-like (https://github.com/plasma-hamper/plasma) pool system implemented in Rust, using **Option A** (serialized writer append via OS/file lock). The v0.0.1 release is CLI-only; the core is an internal implementation detail.

---

## Goals

* **Single-file, mmap-backed pool**: ring buffer of messages stored in a file and memory-mapped for fast read/write.
* **Multiple processes** can attach concurrently:

  * **Many readers** (lock-free reads)
  * **Many writers**, serialized at append time via a cross-process lock
* **No daemon required** for local operation.
* **Crash-tolerant**: readers never interpret torn writes as valid messages; pool can recover to “last committed message.”
* **Zero-copy read path**: a reader can obtain a reference into the mmap’d bytes and interpret it as a Lite³ document without deserialization.
* **Good debugging affordances**:

  * Stable metadata in header (bounds, stats)
  * Optional checksums
  * A validator can scan the file and report corruption/last-good.

## Non-goals (v0.0.1)

* Full behavioral compatibility with legacy Plasma formats/protocols.
* Hard real-time wakeups for followers (we’ll start with polling; futex/notify later).
* Perfect “durability per message” (no `fsync` per append by default).
* Live resize while other processes are attached (resize will require re-open).

---

## High-level architecture

### Crates / modules

v0.1 prefers a single Rust crate (`plasmite`) with internal modules to keep the initial surface area small.
If/when we want stricter separation, we can split into a workspace (`plasmite_core`, `plasmite_cli`, etc.) without changing the on-disk format.

Proposed module layout:

* `core`

  * `pool` (open/create, header parsing, locking)
  * `ring` (frame layout, append, wrap/drop)
  * `cursor` (read iteration, seek-by-seq)
  * `toc` (optional persistent index)
  * `validate` (scan + repair advice)
  * `error` (typed error mapping + CLI exit code mapping)
* `lite3`

  * Lite³ (https://github.com/fastserial/lite3) FFI or native bindings
  * “document view” for zero-copy access
  * encode/decode helpers for CLI/tests
* CLI binary (`plasmite`) calls `core` (CLI design lives in `spec/v0/SPEC.md`).

### Error model (recommended)

In `core::error`, prefer a small set of stable, user-meaningful error variants that map cleanly to CLI exit codes:

* `Internal` → `1` (bug / invariant violation)
* `Usage` → `2` (invalid flags/args; schema violations like non-object `data`)
* `NotFound` → `3` (missing pool, missing seq)
* `AlreadyExists` → `4`
* `Busy` → `5` (lock contention/timeout, “pool in use” cases)
* `Permission` → `6`
* `Corrupt` → `7` (invalid/corrupt pool)
* `Io` → `8` (I/O errors)

Keep the variants coarse; attach rich context for logs/debug output, not for the enum discriminants.

---

## Storage design

### File layout (single file)

```
+----------------------+  0
| PoolHeader (fixed)   |
+----------------------+
| TOC region (optional, fixed-size)   |
+----------------------+
| Ring region (variable-length frames)|
+----------------------+  file_size
```

**Key property:** only the ring region changes frequently; header + TOC are updated in small atomic writes under the writer lock.

### Alignment & endianness

* All structs are `#[repr(C)]`.
* All atomics are **8-byte aligned**.
* Frame starts are aligned to **8 bytes**.
* Endianness: little-endian on disk; reject other endianness (future-proof via header field).

---

## PoolHeader

`PoolHeader` is a fixed-size struct at offset 0 (e.g. 4–8 K, padded).

Fields (conceptual; exact sizing TBD):

* Identity / versioning:

  * `magic = "PLSM"`
  * `format_version: u32`
  * `uuid: [u8;16]`
  * `endianness: u8` (1 = LE)
* Layout:

  * `file_size: u64`
  * `toc_offset: u64`
  * `toc_capacity: u32` (0 => no TOC)
  * `ring_offset: u64`
  * `ring_size: u64`
* Options:

  * `flags: u64` (checksum enabled, sync mode enabled, etc.)
* Global state (atomics; updated by writers, read by readers):

  * `head_off: AtomicU64`  (next write position within ring, relative to ring start)
  * `tail_off: AtomicU64`  (oldest frame start within ring)
  * `newest_seq: AtomicU64`
  * `oldest_seq: AtomicU64` (0 if empty)
  * `msg_count: AtomicU64` (best-effort; exact under writer lock)
  * `used_bytes: AtomicU64` (best-effort; exact under writer lock)
* Locking metadata (v0.1 minimal):

  * none required if we use `flock`/`fcntl` on the file descriptor.
  * reserve space for future robust shared mutex/futex strategy.

**Empty pool invariant:**

* `msg_count == 0`
* `head_off == tail_off`
* `newest_seq < oldest_seq` or `oldest_seq == 0` sentinel

---

## Cross-process locking (Option A)

### v0.1 lock strategy

* Use an **exclusive file lock** on the pool file for append operations:

  * `flock` via `fs2::FileExt::lock_exclusive()` (Unix)
* Lock scope: **only during append** (and during resize).

Why:

* Simple, cross-platform enough (Linux/macOS).
* Microseconds-level overhead, acceptable for room-scale workloads.

### Future optimization (not required)

* Linux-only futex-based shared mutex in the header for faster uncontended lock.

---

## Frame format (ring entries)

Each message is stored as a **frame**:

```
FrameHeader | payload_bytes | padding_to_8
```

### FrameHeader (fixed size, aligned)

Fields:

* `frame_magic: u32` (e.g. "FRM1")
* `state: u32`

  * `0 = EMPTY/INVALID`
  * `1 = WRITING`
  * `2 = COMMITTED`
  * `3 = WRAP` (special marker)
* `flags: u32` (checksum present, etc.)
* `header_len: u32` (for forward compatibility)
* `seq: u64`
* `timestamp_ns: u64`
* `payload_len: u32`
* `payload_len_xor: u32` (`payload_len ^ 0xFFFF_FFFF`)
* `crc32c: u32` (optional; checksum of payload)
* `reserved: [u8; …]` (pad to e.g. 64 bytes)

Header integrity rule:

* A reader must not trust `payload_len` unless `payload_len ^ payload_len_xor == 0xFFFF_FFFF`.

### Payload contents

Payload is a single Lite³ document containing the canonical shape:

```json
{ "meta": { "descrips": ["..."] }, "data": { ... } }
```

`meta.descrips` is always present (empty array if unset).

This preserves the “in-memory == on-disk” feel:

* Reader returns a view into `payload_bytes`
* Lite³ can interpret it directly without copying

### Commit protocol (crash-safety)

Writer (under lock):

1. Reserve space (compute position)
2. Write `FrameHeader` with `state=WRITING`, lengths (including `payload_len_xor`), seq, ts, (crc placeholder)
3. Write payload bytes
4. Compute checksum (if enabled) and write `crc32c`
5. **Release fence**
6. Publish by setting `state=COMMITTED` (single aligned write; last header mutation)
7. Update global header (`head_off`, `newest_seq`, etc.) with **release stores**

Reader:

* A frame is valid only if:

  * `frame_magic` matches
  * `state == COMMITTED` (acquire load)
  * lengths are in-range and frame fits in ring
  * checksum matches (if enabled)
  * header fields are stable across the read (see Read path)

This directly prevents readers from treating torn writes as real messages.

### Durability semantics (power-loss) (future)

Crash-safety (above) is about **not mis-reading torn writes** when a writer crashes or is killed.
Durability is about **how much survives an OS crash or power loss**.

In an mmap-backed design, newly appended bytes typically land in the OS page cache first. That means:

* A message can be **committed** (visible to other processes reading the mmap) but not yet **durable** (guaranteed on storage).
* “Durability” therefore requires an explicit flush policy (or a separate WAL), not just the in-memory commit protocol.

v0.1 position:

* We do **not** force durability per message by default (no automatic `msync`/`fsync` on every append).
* We provide an **opt-in per-append flush** path (CLI: `poke --durability flush`) that flushes the committed frame bytes and header bytes.
* We rely on the crash-safety protocol + conservative validation/recovery to avoid interpreting torn data as valid when running in `fast` mode.

Possible future durability modes (intentionally not part of v0.1 behavior yet):

* **Best-effort (default)**: no explicit flush; fastest; may lose the most recent messages on power loss.
* **Per-append flush**: after committing a frame (and updating the global header), flush the touched ranges to disk.
  * Expected to be significantly slower; good for “audit log” style pools.
* **Periodic flush**: flush every N messages or every T seconds (amortize cost).
* **On-demand flush**: a future `plasmite pool sync` / `plasmite flush` command that forces durability at a chosen time.

Implementation notes when we decide to add this:

* For mmap writes, durability typically means flushing the modified mmap pages (frame bytes + header bytes).
* On Unix, this is usually `msync` (and optionally `fsync`/`fdatasync` for metadata depending on semantics).
* The CLI/API should be explicit about what’s guaranteed (e.g. “durable on storage device” vs “durable in kernel cache”).

---

## Ring behavior (wrap + overwrite)

### Wrap marker

If there isn’t enough contiguous space at the end of the ring for a new frame:

* writer writes a `WRAP` frame at the current `head_off` (committed immediately)
* sets `head_off = 0`
* continues writing the actual message at the start

Readers encountering a `WRAP` frame jump to offset 0.

### Free space calculation

Let:

* `H = head_off`, `T = tail_off`, `R = ring_size`
* Ring is “used” between T→H (wrapping allowed)

Free space depends on relative order:

* If `H >= T`: used = `H - T`, free = `R - used`
* If `H < T`: used = `R - (T - H)`, free = `R - used`

(We also maintain `used_bytes` under lock as the source of truth; the above is for validation.)

### Overwrite policy (default)

Default: **drop oldest to make room** (like a bounded log).

Under writer lock:

* Required space = `frame_total_size` (+ possible `WRAP` header)
* While free < required:

  * Read frame at `tail_off`
  * If `WRAP`, set `tail_off = 0` and continue
  * Else if committed frame:

    * advance `tail_off += frame_total_size`
    * `oldest_seq = oldest_seq + 1` (or set to next frame’s seq if gaps possible)
    * decrement `msg_count`, subtract from `used_bytes`
  * If invalid/corrupt at tail: stop and mark pool as corrupt (see Recovery)

**Note:** With Option A, only the writer mutates `tail_off`, but readers must still observe `tail_off` to detect “fell behind” under overwrite.

---

## Correctness model under overwrite (normative)

### What we guarantee

1. Writers never interleave (serialized by the append lock).
2. A reader never treats a partially-written frame as valid (`state` is the publish gate).
3. A reader never returns a frame that was overwritten mid-read as valid (stable-read validation).
4. Readers are lock-free and best-effort under overwrite:

   * If a reader falls behind and data is dropped, the reader detects it and resyncs.
   * A reader may miss messages that were overwritten before it could consume them.

### What we do not guarantee (without a consumer registry)

* “Every reader sees every message.” Overwrite means slow readers will drop data by design.

---

## Optional persistent TOC (recommended)

Without a TOC, `get(seq)` and `tail N` can devolve to O(n) scans. A small persistent TOC gives “good enough” random access without reintroducing complexity.

### TOC region

A fixed-size circular array of `TocEntry` structs, `toc_capacity` entries.

`TocEntry` fields:

* `seq: u64`
* `offset: u64` (ring-relative)
* `payload_len: u32`
* `timestamp_ns: u64`
* `flags: u32`
* `crc32c: u32` (optional)
* `state: u32` (valid bit)

Header has:

* `toc_head: AtomicU32` (next write index)
* `toc_tail: AtomicU32` (oldest entry index)

Writer append:

* write entry at `toc_head`
* advance `toc_head` (wrap)
* if overwriting, advance `toc_tail`

Reader operations:

* `get(seq)`: scan TOC ring (O(capacity)) or build an in-memory hash map lazily
* `tail N`: walk backwards from newest toc entries

v0.1 recommendation:

* Keep TOC scan linear; cap default to something reasonable (e.g. 4096–65536).
* Later: build an in-memory index cache per process.

---

## Open & recovery

### Fast open path (normal)

On open, map file, read header, validate:

* magic/version/endianness
* ring bounds sane
* head/tail within ring_size

No full scan required.

### Validation / repair mode (tooling + library helper)

Provide `validate_pool()` that can scan from `tail_off` to `head_off`:

* count committed frames
* verify checksums
* locate last committed seq
* detect:

  * torn write at head (expected after crash)
  * corruption in middle (unexpected)

Recovery behavior:

* **Expected crash case**: last frame is `WRITING` or invalid at head:

  * treat it as end-of-log; do not advance head past last committed
  * (optionally) writer can “rewind head” under lock at next append
* **Corruption in middle**:

  * mark pool “corrupt” flag in header (under lock)
  * require manual intervention (or best-effort salvage: re-tail to next valid frame)

In v0.1, keep recovery conservative:

* tolerate partial tailing at the end
* do not attempt complex repair heuristics automatically unless explicitly requested.

---

## Read path (cursor)

### Cursor state

A reader cursor is `(next_off, next_seq_expected?)`.

Operations:

* `seek_to_seq(seq)`:

  * If TOC present: find offset by TOC; else scan forward from tail until seq match.
* `next()`:

  * Read frame at `next_off`:

    * if WRAP -> `next_off=0`, continue
    * if COMMITTED -> validate stable snapshot, then return `MessageRef` and advance by frame size
    * if WRITING/EMPTY -> “no new message” (for follow mode)

#### Frame read validation under overwrite (normative)

Because writers may drop-oldest and overwrite ring regions, the read path is an optimistic snapshot problem. A checksum helps detect torn payloads, but it does not by itself prevent a reader from accepting bytes that were overwritten while it was reading.

Algorithm (“double-header / stable snapshot”):

Given `O = cursor.next_off`:

0. Check you haven’t fallen behind.

   * Load `tail_off` (acquire) from the PoolHeader.
   * If `O` is no longer within the readable region (i.e. it is “before tail” in ring order), the reader fell behind due to overwrite:

     * set `O = tail_off` and return `FellBehind` (or continue silently, depending on caller needs).

1. Read header (H1).

   * Copy the fixed header bytes into a local struct `H1`.
   * Validate header sanity before any payload access:

     * `frame_magic` matches
     * `state == COMMITTED` (or `state == WRAP`)
     * `header_len` matches expected (v0.1 fixed)
     * `payload_len ^ payload_len_xor == 0xFFFF_FFFF`
     * `payload_len <= MAX_PAYLOAD` (see guardrails below)
     * `frame_total_len = align8(header_len + payload_len)` fits within ring bounds

   If any sanity check fails: treat as invalid/overwritten, resync to `tail_off`, and retry.

2. Read payload bytes using `H1.payload_len`.

   * If checksum is enabled, compute checksum over the payload bytes.

3. Re-read header (H2).

   * Copy the header bytes again into `H2`.

4. Accept only if stable.

   Accept the frame IFF all of the following hold:

   * `H2.frame_magic == H1.frame_magic`
   * `H2.state == H1.state == COMMITTED` (or both are WRAP)
   * `H2.seq == H1.seq`
   * `H2.payload_len == H1.payload_len`
   * `H2.payload_len_xor == H1.payload_len_xor`
   * `H2.timestamp_ns == H1.timestamp_ns`
   * `H2.flags == H1.flags`
   * (if checksum enabled) `H2.crc32c == H1.crc32c` and computed checksum matches

   If any mismatch: the frame was overwritten or mutated during the read. Discard it, reload `tail_off`, and retry (resyncing to tail is the fastest safe move).

5. Advance cursor.

   * If `state == WRAP`, set `O = 0` and continue.
   * Else advance `O += frame_total_len`.

##### Guardrails (normative)

1. Hard cap payload length.

   Define `MAX_PAYLOAD` as `min(ring_size - header_len, MAX_PAYLOAD_ABS)` where `MAX_PAYLOAD_ABS` is an absolute cap (e.g. `256M`). This prevents a torn header from causing out-of-bounds reads.

2. Publish gate ordering.

   Writer must not set `state=COMMITTED` until the entire payload and checksum fields are fully written.

##### Why checksums alone aren’t sufficient

A checksum tells you “these payload bytes match what some writer intended,” but a reader can still pair a valid payload with the wrong header if it races an overwrite. The double-header stability check ensures the payload corresponds to the same committed frame instance whose header was observed.

#### Cursor semantics under overwrite (recommended API)

Expose overwrite reality in the cursor API:

* `Ok(MessageRef)` (stable committed frame)
* `Err(WouldBlock)` (no committed frame at current offset; in follow mode caller waits)
* `Err(FellBehind { new_from_seq })` (cursor offset was overwritten; caller may log and resume)
* `Err(Corrupt)` (validator-level issue)

### Zero-copy message reference

`MessageRef<'a>` contains:

* `seq`, `ts`, `flags`
* `payload: &'a [u8]` (slice into mmap)
* `lite3_view: Lite3DocRef<'a>` (constructed on demand)

No allocations required unless the caller asks for JSON conversion.

---

## Follow / waiting strategy

v0.1 uses polling:

* Reader checks `newest_seq` or attempts `next()`
* If nothing new:

  * sleep with backoff (e.g. 100µs → 1ms → 5ms up to a cap)
  * reset backoff on new message

Future:

* Linux futex wait on `newest_seq`
* macOS: keep polling (or explore platform primitives later)

---

## Resize

`pool resize`:

* requires exclusive lock
* `ftruncate` to new size
* update header: `file_size`, `ring_size`, etc.
* **Processes must re-open** to see new mapping (documented limitation)

v0.1: safe approach is “resize only when no other processes are attached” (best-effort; we can’t reliably detect all attachers without a registry).

---

## Lite³ integration

### Storage format choice

We store **one Lite³ document per message**:

* avoids multiple payload segments
* keeps `MessageRef` simple

### Writing

* Core expects `payload_bytes` already in Lite³ format.
* Convenience helpers:

  * from `serde_json::Value` → Lite³ bytes (CLI usage)
  * from meta + data JSON → canonical structure

### Reading

* Core returns the raw bytes and/or a Lite³ view.
* CLI can render to JSON via Lite³ decode.

---

## Testing plan (implementation)

### Unit tests (pure Rust)

* Frame encoding/decoding:

  * alignment, size computation, wrap marker behavior
* Free space math:

  * head>=tail, head<tail cases
* Drop-oldest loop:

  * ensure it terminates, preserves invariants
* Header invariants:

  * empty pool, single message, wrap, overwrite

### Property tests

* Generate random sequences of operations:

  * append random payload sizes
  * occasionally force wrap by making payload large
  * occasionally force overwrite by limiting pool size
* Verify:

  * cursor iteration yields a contiguous stream of committed frames
  * bounds (`oldest_seq..=newest_seq`) match observed frames
  * no out-of-bounds reads occur

### Multi-process integration tests

Spawn multiple OS processes (not threads):

* Many readers + many writers appending
* Verify:

  * writers never corrupt the pool
  * readers never crash / never parse invalid frames
  * eventual consistency: readers observe all committed seqs (minus overwritten)

### Crash/recovery tests

* Writer process acquires lock, begins frame write, dies mid-write (SIGKILL).
* New reader attaches:

  * must not treat partial frame as committed
* New writer appends:

  * should recover by overwriting the “dangling head” region safely (under lock)
* Add checksum-on mode to detect torn payloads.

### Fuzzing (optional but valuable)

* Fuzz the validator against mutated pool files:

  * ensure it never panics, always bounds-checks
  * gives useful corruption location reporting

---

## Performance expectations

* Append cost dominated by:

  * file lock acquire/release (microseconds)
  * memcpy of payload into ring
  * optional checksum computation
* Read cost is near-zero-copy:

  * parse Lite³ view lazily
  * conversions to JSON are for tooling only

---

## Risks & mitigations

1. **Cross-process atomics + mmap correctness**

   * Mitigation: strict alignment, `Atomic*` for header state, acquire/release discipline, validator asserts.
2. **Corruption edge cases**

   * Mitigation: checksum option, conservative recovery, strong validator.
3. **Follow latency with polling**

   * Mitigation: backoff strategy; add futex waiting later if needed.
4. **Resize semantics**

   * Mitigation: require re-open; document; keep tool simple.

---

## Open decisions (we can settle together quickly)

1. **Default overwrite policy**

   * Drop-oldest (recommended) vs block-writers (could be optional).
2. **Default TOC capacity**

   * None (simpler, slower random access) vs modest default (better UX).
3. **Checksum default**

   * Off by default for speed, but easy to enable per pool.
4. **Durability “sync mode”**

   * Provide `sync=true` option (flush on commit) vs omit in v0.1 (see “Durability semantics” above).
