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
These measurements support a small throughput gain on this host. Resource
pressure and idle CPU measurements remain pending for the writer-handle ticket.

## Correctness checks

Differential tests compare binary validation with the old JSON exporter for all
65,536 two-byte prefixes in both keys and values, Unicode boundary cases,
non-finite and extreme finite floats, and native byte fallback. Existing tests
cover malformed child links, depth, tags, canonical types and rejection before
sequence advance. Full metadata callers still receive owned tags. Full message
callers retain JSON decoding. No entry point gains stronger validation merely
to share this code.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
