# Binary validation and writer handles

A producer that already has Lite3 bytes should validate those bytes without
printing their contents as JSON. The new path reuses the existing checked C
traversal, adds the UTF-8 and finite-number checks that the JSON writer enforces,
and discards no temporary JSON. Native byte values keep the old exporter as a
fallback. Validation still visits the data; its cost grows with payload size.

The Rust raw tail also borrows tag strings when no filter needs owned tags.
Ordinary C and binding raw reads already skip JSON validation, so their read
path gains nothing from this change.

## Complete producer and consumer measurements

The Python benchmark runs separate producer and consumer processes against
optimized libraries on macOS 27.2, Apple M5 Pro, Python 3.15.2. Both participants
finish before timing ends. Setup, imports and fixture encoding sit outside the
measurement. Fast durability retains history without forcing each write to disk.
The consumer checks every sequence, complete binary payload and timestamp.
The producer checks every committed sequence. Each library receives the same
fixture. Runs alternate libraries and record combined process CPU.

| Workload | Repeats | Baseline messages/s | Candidate messages/s | Baseline/candidate CPU seconds |
| --- | ---: | ---: | ---: | ---: |
| 100,000 small binary messages | 7 | 113,070 | 139,652 | 1.062361 / 1.017888 |
| 5,000 binary messages with 64 KiB text | 5 | 30,781 | 42,307 | 0.220311 / 0.164192 |
| 100,000 complete JSON objects | 5 | 49,107 | 48,928 | 3.287589 / 3.280750 |

Binary throughput rises 23.5% for small messages and 37.4% for large messages.
Combined CPU falls 4.2% and 25.5%. The complete-object control changes less than
0.4% in throughput. These measurements cover validated binary append plus the
binding raw consumer; they do not isolate Rust raw-tail or tag-filter costs.
They repeat one pre-encoded record. They do not compare binary work with JSON
pipe work or establish performance on another operating system.

[Small raw samples](binary-small.raw.json), [large raw samples](binary-large.raw.json)
and [JSON control](json-control.raw.json) include toolchain, script/binding/library
hashes and worker verification counts. The small run used the binary benchmark
before its later configurable text size and dynamic pool capacity changes;
its script hash records that distinction. The large run uses the current script.
The no-cache baseline comes from commit 53a6bcd. For validation, the baseline
adds the writer-handle change below (commit `1e29a6a`) and the candidate adds
the change titled `Validate binary message readability without exporting JSON`.
Use the exact SHA-256 fields in each raw file to identify these builds.
Both libraries use the same vendored source and optimized build profile.

Reproduce with `scripts/bench_pipe_comparison.py --binary --messages 100000
--repeats 7 --library CANDIDATE --baseline-library BASELINE --output RESULT`.
Set `PLASMITE_BENCH_NOTE_BYTES=65536` and use 5,000 messages for the large run.
Without `--binary`, the script checks complete object receipts and consumer data
and includes the JSON pipe. The pipe provides a different retention contract.

## Writer handle reuse

An open Pool now lazily retains one semaphore handle after its first successful
append notification. Each subsequent append posts that handle after publishing
the record and releasing the writer lock. A failed open leaves the cache empty;
a failed post drops the handle so the next append retries. Pool drop closes its
reference. Read-only pools acquire no writer handle. Unsupported platforms retain
the existing polling fallback. The cache adds no thread, timer or idle operation.

The [seven-repeat ordinary JSON comparison](writer-handles.raw.json) shows
47,385 to 48,811 messages/s, a 3.0% gain. Combined CPU changes from 3.296423 to
3.321993 seconds. The [paced control](writer-handles-paced.raw.json) shows the
same p95 delivery latency, about 7.63 ms. Handle reuse removes one open and close
per append after the first; it does not change the reader's timed polling.
These measurements support a small throughput gain on this host.

The [resource experiment](writer-resources.raw.json) runs three open/write/close
cycles with 64 pools per library. Both start with four process descriptors and
reach 68 after opening pools. The baseline remains at 68 after writing; handle
reuse reaches 132. Every close returns to four, including after reopen and
another write. On macOS, the cache therefore retains one additional descriptor
per pool that has written successfully. Read-only pools retain none. Applications
with many writable pools must include this descriptor in their resource budget;
the experiment's soft limit was 1,048,575 descriptors. The cache has no global
owner and closes each reference with its Pool.

Across the three approximately one-second idle samples, baseline process CPU
ranges from 36 to 44 microseconds and candidate CPU from 33 to 46 microseconds.
These values sit at the measurement floor and show no new idle work. This tests
idle writers with open handles, not reader wake-up behavior. Reader waiting code
is unchanged. [writer-resources.py](writer-resources.py) records descriptor
counts, the process limit, library hashes and idle CPU; pass its `--bindings`,
`--baseline-library`, `--library` and `--output` arguments to repeat it. It removes
only its own closed test semaphore names before deleting its temporary files.

The native ownership test also closes/reopens the cached reference, moves its
sole owner to another thread and confirms the independent receiver still gets
all posted signals. Existing cross-process producer/consumer, pool identity,
notification-failure fallback and multiple-reader tests retain their coverage.

## Correctness checks

Differential tests compare binary validation with the old JSON exporter for all
65,536 two-byte prefixes in both keys and values, Unicode boundary cases,
non-finite and extreme finite floats, and native byte fallback. Existing tests
cover malformed child links, depth, tags, canonical types and rejection before
sequence advance. Full metadata callers still receive owned tags. Full message
callers retain JSON decoding. No entry point gains stronger validation merely
to share this code.

## Rust read-side selection

Seven alternating optimized repeats compare commit `1e29a6a` with binary
validation commit `5dcaa4d` against the same prepared pool bytes. Each pool keeps
all records and indexes every sequence. The consumer warms the complete history,
then times another complete pass. Every mode checks exact sequence, timestamp
and payload or complete decoded data. Timings include snapshot locking/copying,
selection, verification and tail creation/close. They exclude pool preparation,
process startup, pool open and warmup. This isolates consumption; the concurrent
producer/consumer results above establish the combined binary pipeline gain.

| Consumer / fixture | Records | Baseline ms | Candidate ms |
| --- | ---: | ---: | ---: |
| Raw tail / sensor | 100,000 | 209.814 | 99.708 |
| Raw tail / wide | 20,000 | 195.714 | 67.692 |
| Raw tail / large text | 5,000 | 105.344 | 36.646 |
| Tag-filtered raw / sensor | 100,000 | 209.755 | 106.637 |
| Tag-filtered raw / wide | 20,000 | 195.577 | 69.322 |
| Tag-filtered raw / large text | 5,000 | 104.158 | 37.718 |
| Metadata point read / sensor | 100,000 | 212.213 | 108.665 |
| Metadata point read / wide | 20,000 | 194.303 | 69.628 |
| Metadata point read / large text | 5,000 | 103.768 | 36.145 |

Raw tails take 52–65% less elapsed time. Tag-filtered tails and metadata reads
show similar reductions. Separate allocation runs count Rust allocator requests,
including snapshots: raw tails fall from 10 to 5 calls per record; filtered tails
and metadata reads fall from 10 to 8. Large raw reads request 133,060 versus
67,053 Rust allocation bytes per record. These counters exclude native C
allocations and count requested bytes across an operation, not live heap size.

The ordinary JSON control takes 348.770/350.619 ms for sensors,
423.917/435.107 ms for wide and 116.878/116.286 ms for large text. The wide
control is 2.6% slower; the others differ by about 0.5%. Snapshot-only point-read
controls differ by at most 1.2%. JSON allocation counts remain identical. The
binary gain does not imply faster full-object decoding. CPU medians, process
peak resident memory, allocation diagnostics and every repeat live in
[selection.raw.json](selection.raw.json).

[selection-bench.rs](selection-bench.rs) implements the consumer;
[selection-bench.py](selection-bench.py) prepares each history once, alternates
binaries and runs allocation diagnostics separately. It removes its closed
semaphore name and temporary pool after each fixture. Fixtures come from the
encoding measurement folder, but this comparison uses the earlier encoding
implementation in both builds. `get` controls exercise a complete index to
avoid measuring history scans instead of the selection change.

Build each archived commit with its own `CARGO_TARGET_DIR`; sharing a target
between archived trees can reuse an earlier crate when archive timestamps
precede the previous build. An initial shared-target candidate reused the old
code; it supplied no timed samples. Final builds use isolated directories.
Verify the candidate contains `plasmite_lite3_verify_json` and the baseline does
not, then compile the same helper against each release rlib and its matching
`libc`/`serde_json` dependencies. Use `rustc --edition=2024 -O`, pass the rlibs
with `--extern`, and pass that target's release/deps with `-L dependency=...`.
The raw record includes both executable hashes, helper/runner hashes, compiler,
input/encoded-payload hashes and exact verified counts.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
