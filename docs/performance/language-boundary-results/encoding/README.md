# Parse input once and estimate binary structural overhead

The encoder now keeps one owned parsed JSON document through output-capacity
retries. It also uses that document's existing value count to estimate initial
space: JSON bytes plus 32 bytes per parsed value and 64 bytes of root overhead,
with the existing minimum and maximum. This estimate separates text size from
binary structural cost. It does not guarantee a single attempt. Dense arrays
can still grow geometrically; the encoder always attempts the exact cap.

The native parser already records the value count, so reading it adds no walk.
The encoder calls the same pinned recursive Lite3 builders and starts every
attempt with a fresh zeroed output buffer. A failed insertion can leave partial
state, so the design rebuilds from the retained document rather than continuing
inside damaged output. A Rust owner frees the document on success, failure or
unwind. Each operation owns its allocations and retains no scratch cache.

The shim uses two private recursive symbols from the pinned decoder. The
upstream top-level wrapper frees its document after one attempt; these builders
borrow it. The local dispatch mirrors that wrapper without its free. This adds
a native ownership boundary and must follow any future vendor upgrade. It does
not add another codec, parser, storage format or public API. The legacy scalar
root's empty-output behavior remains unchanged; canonical messages separately
require an object.

## Complete pipeline results

Alternating optimized runs use separate Python producer and consumer processes
on macOS 27.2, Apple M5 Pro, Python 3.15.2. All messages retain full data, and the
consumer decodes and verifies every record. Both processes finish before timing
ends. Setup, imports and fixture construction stay outside timing. Fast
durability and retained history match between libraries. Baseline is commit
`7db63b5`. The candidate changes only parsing lifetime and the size estimate.

| Requested producer result | Records/repeats | Baseline messages/s | Candidate messages/s | Combined CPU baseline/candidate s |
| --- | ---: | ---: | ---: | ---: |
| Full receipt, sensor | 100,000 / 7 | 48,449 | 49,718 | 3.308218 / 3.312169 |
| Full receipt, 200 extra fields | 20,000 / 5 | 12,863 | 13,508 | 2.528533 / 2.475026 |
| Full receipt, 64 KiB text | 5,000 / 5 | 6,503 | 6,417 | 1.207162 / 1.220290 |
| Numeric receipt, sensor | 100,000 / 7 | 74,466 | 79,685 | 2.493697 / 2.381556 |

Numeric receipt throughput increases 7.0%, with 4.5% less combined CPU. Full
receipt throughput increases 2.6% and 5.0% for sensor and wide records; wide CPU
falls 2.1%. The large-text control loses 1.3% throughput and uses 1.1% more CPU.
Do not treat small changes as universal gains. Full receipt serialization still
dominates much of the ordinary producer work. Numeric receipt producers expose
more of the encoder cost, until the consumer becomes the next limit.

Raw files named `parse-sized-*` preserve these measurements, hashes, worker CPU,
verification counts and individual process peak resident memory. Each process's
peak includes imports, mapping/setup and shared mapped pages; summing those
peaks does not yield simultaneous private memory use. This is a whole-process
regression check, not a live-native-allocation accounting method. The measured
resident peaks remain close between variants; the allocation diagnostic below
shows the more precise output-capacity tradeoff.

## Compare simpler alternatives

Keeping the original two-times-text initial estimate while parsing once gains
1.0% on sensors and 2.2% on wide records. Its separate large-text run gains 1.5%,
though that shape already uses one parse; these small controls show timing
variation, not a removed-parse explanation. See `parse-once-*.raw.json` and
[its source patch](parse-once-2x.patch).

A baseline encoder with only a four-times-text estimate gains 2.7% sensor
throughput and increases combined CPU 0.4% in its own seven-repeat comparison
(`sizing-small.raw.json`). It avoids that fixture's retry but reserves much more
space for strings. Its source change is the single initial multiplier 2 to 4
in baseline `Lite3Buf::from_json_str_with_limit`. The value-count estimate earns
its extra ownership code through the numeric producer gain and the string
allocation reduction, while preserving the existing bounded retry path.

## Allocation diagnostic

[allocation-diagnostic.c](allocation-diagnostic.c) runs the actual pinned
builders for four strategies and checks exact encoded bytes between them.
[allocation.raw.json](allocation.raw.json) records every capacity attempt,
requested output bytes, actual macOS allocation sizes, document allocation
footprint and parse counts. These are allocation diagnostics, not elapsed-time
measurements. Document footprint includes its value and string allocations.
Output totals sum allocations across attempts, not their simultaneous peak.
The fixtures include sensor, wide, large text and two scaled 64 KiB cap cases.

| Fixture | Baseline parses / candidate parses | Baseline / candidate output requests | Baseline / candidate final physical allocation |
| --- | ---: | ---: | ---: |
| Sensor | 2 / 1 | 5,430 / 2,857 B | 4,096 / 3,072 B |
| Wide | 2 / 1 | 23,376 / 18,712 B | 16,384 / 20,480 B |
| Large text | 1 / 1 | 131,932 / 67,918 B | 147,456 / 81,920 B |
| Array near scaled cap | 4 / 1 | 135,550 / 102,633 B | 65,536 / 65,536 B |
| String near scaled cap | 1 / 1 | 65,536 / 40,168 B | 65,536 / 49,152 B |

The wide fixture reserves 4 KiB more in its final output allocation, while
requesting fewer bytes across retries. Large text uses 44% less final physical
allocation. The four-times-text alternative reserves 278,528 B for the same
large fixture. These macOS allocator sizes do not establish other platforms'
size classes or the application's total retained-memory behavior.

## Reproduction and checks

[bench.py](bench.py) preserves the exact measured harness. Prefer the repository
script, or pass `--bindings` when running this saved copy. Compare saved optimized
library directories with `--library CANDIDATE --baseline-library BASELINE`.
Use the records/repeats in the table. Set `PLASMITE_BENCH_EXTRA_FIELDS=200` for
wide and `PLASMITE_BENCH_NOTE_BYTES=65536` for large. For the numeric control,
set both `--receipt identity --baseline-receipt identity`. No build or other
benchmark runs concurrently with timed trials. Raw files record exact library,
binding and script hashes.

For allocation diagnostics, build the candidate library, then link the C program
and `c/lite3_shim.c` with its `liblite3.a` from
`target/release/build/plasmite-*/out/`. Include `c`, `vendor/lite3/include` and
`vendor/lite3/lib`; use `cc -std=gnu11 -O2`. On this macOS host it uses
`malloc_size`. Run with a fixture path and cap: 268435456 normally, 65536 for
`*-limit.json`. The test fixtures preserve the diagnostic input bytes; they do
not establish execution at the full 256 MiB cap.

Rust tests compare byte-for-byte output with the old decoder for scalar, array,
wide/nested, Unicode, NUL-escape, numeric-limit and large-string cases. Tests also
cover exact cap attempts, rejected oversized input followed by 100 small calls,
bounded input borrowing and four independent concurrent encoders. The same
parsed document feeds every retry; output ownership stays in Rust.
