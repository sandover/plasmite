# Format stored values directly into JSON

C JSON get/tail now formats an owned Lite3 snapshot directly. The caller asks
for JSON, so the adapter borrows the stored values while writing that result.
It builds neither a yyjson document nor an owned Rust application-data tree.
Python, Go and Node retain their existing object/JSON APIs and buffer ownership.

## The work that disappeared

The previous adapter reused Rust's object API as an internal bridge:

```text
Owned Lite3 frame
  -> yyjson document -> data JSON -> copied UTF-8 String
  -> Rust Value tree -> complete envelope JSON
```

The direct path uses the same cursor, owned snapshot and frame identity:

```text
Owned Lite3 frame -> checked borrowed values -> complete envelope JSON
```

The private shim supplies native value views and collection entries. One checked
binary walk verifies structure, UTF-8 and finite floats before formatting. The
formatter borrows strings and tags, writes stored integer/float values directly,
and streams native byte fields as base64. Temporary entry lists preserve stable
lexical key order and last-duplicate-wins behavior without owning key strings.
The existing wire type accepts borrowed serializable data and tags; it still
owns the canonical envelope field names and order.

The stable snapshot copy still protects readers from pool overwrite and close.
Validation, collection traversal, key ordering, timestamp formatting, output
buffer allocation and final runtime object construction still cost work. The
ordinary Python API still parses JSON and constructs its Message/datetime
objects. These measurements concern the removed native bridge, rather than a
claim that ordinary object reads require no copies or allocations.

## Floating-point values come from storage

A stored binary64 float already has its bits. Formatting and parsing it again
to recreate an adapter's rounding adds work and can change that value.

The pinned serde_json 1.0.149 default parser first converts a decimal significand
to f64, then multiplies or divides it by a floating-point power of ten. Those
separate rounding operations can choose a different adjacent float from a
correctly rounded conversion of the original decimal. yyjson's emitted text
can round-trip the stored value while that default Rust parser changes it.
For example, stored `2.027716612057172e155` became
`2.0277166120571721e155` through the extra parse.

Rust now enables serde_json's `float_roundtrip` feature. Object reads preserve
the stored float, and direct JSON output formats it once. This corrects the
observable result; it deliberately removes the old parser's extra rounding.
The [migration note](../../../record/upgrading-2.0.md#json-reads-preserve-stored-floating-point-values)
records that policy and the existing signed-integer/binary64 payload range.
Exact unsigned 64-bit envelope identity keeps its separate representation.

[splice-corpus.rs](splice-corpus.rs) generates differences that expose the issue in the original
parse-and-reserialize path. Five explicit records plus finite floats from 5,000
seeded random bit patterns produce 4,998 cases, 3,176 different output strings
and 1,517 different decoded float values. These exponent-spanning/adversarial
values diagnose the rounding mechanism; they do not estimate the frequency in
ordinary application data. [Metadata](splice-corpus-metadata.json) records the
seed, parser features and hashes. Reproduce with serde_json 1.0.149 default
features and the shim archive from commit `a0735d8`, before the float fix.

The permanent tests compare direct formatting with a correctly rounded reference
over 5,000 generated floats and explicit extremes, Unicode, escapes and negative
zero. Another 10,000-value test checks stored float bits through object/JSON
round trips. Native-byte tests compare base64 output across 512 lengths and byte
patterns. Envelope tests cover maximum identity/time, tags, native bytes,
malformed payloads and stable error kinds, and confirm zero data-tree decodes or
whole-data JSON exports in the direct route.

## Matched measurements

Both measured libraries enable the corrected float parser. The baseline otherwise
uses commit `a0735d8`; the candidate adds direct JSON formatting. This separates
the read-path improvement from a numeric-policy/configuration change. Both use
the same Cargo.lock and optimized profile, with separate Cargo targets for the
two source trees. [environment.json](environment.json) records hashes and checks.
The measurements cover this macOS host; Linux/Windows need their own timing.

The [read-only harness](reader.py) prepares one immutable history, then runs
ordinary Python tails against the same bytes. Each consumer constructs full
Message objects and verifies every complete payload and its sequential id.
Imports and history preparation stay outside timing. One full warm-up per path
precedes alternating measured runs. Small records use 50,000 messages and seven
repeats; large and wide use 10,000/20,000 messages and five repeats.

| Ordinary full-object reader | Records | Previous / direct ms | Previous / direct CPU s |
| --- | ---: | ---: | ---: |
| Small | 50,000 | 504.844 / 418.865 | 0.504290 / 0.418501 |
| 16 KiB text | 10,000 | 302.590 / 252.629 | 0.302324 / 0.249561 |
| 200 extra fields | 20,000 | 955.203 / 699.799 | 0.952795 / 0.697351 |

Small/large readers take 17%/17% less elapsed time; wide readers take 27% less.
Their CPU falls by similar amounts. Raw reports: [small](reader.raw.json),
[large](reader-large.raw.json), [wide](reader-wide.raw.json).

The concurrent [pipeline harness](bench.py) includes producer encoding, committed
receipts, native reads and complete consumer decoding/verification. Small runs
use 100,000 messages; large/wide use 10,000/20,000. Numeric small uses seven
alternating repeats; other modes use five. Fast durability, sufficient retained
history and caller work match. No builds or other experiments run concurrently.

| Complete producer/consumer work | Previous / direct messages/s | Previous / direct CPU s |
| --- | ---: | ---: |
| [Small, numeric receipt](identity.raw.json) | 75,954 / 75,064 | 2.483250 / 2.365651 |
| [Small, complete receipt](full.raw.json) | 47,373 / 46,775 | 3.555492 / 3.476716 |
| [16 KiB text, numeric receipt](large.raw.json) | 30,314 / 29,509 | 0.637574 / 0.618755 |
| [200 extra fields, numeric receipt](wide.raw.json) | 20,490 / 22,610 | 1.817883 / 1.644627 |
| [Binary forwarding control](binary.raw.json) | 140,144 / 138,772 | 0.998474 / 1.006537 |

Wide-record throughput rises 10.3%, with 9.5% less combined CPU. Small/large
throughput medians fall 1.2–2.7% while CPU falls 2.2–4.7%; the unchanged binary
control varies by about 1%. These results support a reader CPU/time gain,
rather than a uniform throughput gain for every complete pipeline.

The small and large pipelines remain producer-limited: their producer still
parses JSON into Rust Value, serializes an envelope, and asks yyjson to parse it
again. Faster reading reduces combined CPU while producer cost limits elapsed
time. Wide records expose more removable reader allocation. The binary control
bypasses JSON; variation there helps bound small throughput claims. Keep all
repeats and per-worker CPU alongside the medians.

## Allocation diagnostic

[allocation-probe.c](allocation-probe.c) counts calls to malloc/calloc/realloc
and cumulative requested bytes in controlled reader workers. A separate
calibration sees one 123-byte allocation and one free. The read scope includes
native C/Rust work and Python's matching runtime work; it does not count all
logical Python allocations or track live/peak allocator bytes. Diagnostic timing
never serves as a speed result.

| 10,000 verified reads | Previous / direct allocation calls | Previous / direct requested bytes |
| --- | ---: | ---: |
| [Small](allocations.raw.json) | 650,023 / 260,023 | 231,029,189 / 110,454,005 |
| [200 extra fields](allocations-wide.raw.json) | 3,060,023 / 370,023 | 1,081,449,115 / 691,363,931 |

The small reader removes 39 allocator entries per record (60%); the wide reader
removes 269 (88%). Cumulative requested bytes fall about 52% and 36%. These
counts include allocator reuse/growth requests and do not represent live bytes.

Peak resident memory in the ordinary runs includes mapped history and process
setup. It remains close between paths and cannot establish a per-message live
allocation size. The allocation diagnostic instead identifies the transient
work that disappeared.

## Reproduce and validate

Build the candidate from the commit titled `Format owned Lite3 frames directly
into JSON responses`. Use an isolated `CARGO_TARGET_DIR` and save the optimized
library.
For the baseline, archive `a0735d8`, enable only serde_json's `float_roundtrip`
feature, and build in another target directory. Use one common Python binding.
The source snapshots and raw files preserve the exact workload and library hashes.

Run `bench.py` with both library directories and `--bindings bindings/python`.
Use `--receipt identity --baseline-receipt identity` for numeric receipts,
`--binary` for the raw control, and defaults for complete receipts. Large records
set `PLASMITE_BENCH_NOTE_BYTES=16384`; wide records set
`PLASMITE_BENCH_EXTRA_FIELDS=200`. Pass the same directories, bindings and
`--harness bench.py` to `reader.py`. Compile the allocation probe with
`cc -dynamiclib -std=gnu11 -O2 -Wall -Wextra` and add `--native-counts` only for
separate diagnostics.

`just check` and `just bindings-test` pass, including all Rust/integration tests,
Go packages, 51 Python tests (one existing skip), 51 Node tests, packaged secure
server start/restart, remote-only packaging and TypeScript checks. No release or
publication forms part of this change.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
