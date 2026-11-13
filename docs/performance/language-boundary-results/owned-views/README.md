# Owned views: baseline and first measured costs

The legacy parse-once candidate improves the complete Python JSON pipeline.
The first strict append-view path still costs more than identity receipts on
small and long-string records. Cold views help larger selected reads, while
small selected reads regress. These results guide the next implementation work;
they do not establish a faster Python or JavaScript view API.

## Builds and workload

The baseline comes from `f79552fea8b00080e8654d35148054aae6931f9f`.
The candidate includes the parse-once change `d959fc5` and the first shared
owned-view/aligned-writer work. [Environment and source fingerprints](environment.json)
identify the compiled files and artifacts. The candidate runtime source remained
unchanged through both builds and all measurements. The source overlay archive
contains the measured Rust/C/header/example sources; apply it over the baseline
checkout when reproducing this intermediate state.

The host runs macOS on Apple M5 Pro with Python 3.15.2. Five alternating measured
repeats follow one warmup. Each pipeline uses independent producer and consumer
processes, Fast durability, and complete value/sequence verification. Small runs
use 20,000 records, wide runs use 5,000 records with 200 extra fields, and large
runs use 2,000 records with 64 KiB strings. Fixture creation, imports and process
startup sit outside elapsed delivery time. Builds/tests stopped during timing.
Raw files retain library/binding/runner hashes, order, counts, CPU and worker RSS.

## Legacy JSON paths against the baseline

The producer requests the same result on both builds. Both consumers decode and
verify complete native data. Full producers also verify the full echoed data;
identity producers check only committed identity.

| Workload/result | Baseline messages/s | Candidate messages/s | Throughput change | Combined CPU change |
| --- | ---: | ---: | ---: | ---: |
| Small/full | 47,763 | 57,569 | +20.5% | −13.9% |
| Wide/full | 13,003 | 17,187 | +32.2% | −15.0% |
| Large/full | 6,391 | 7,603 | +19.0% | −10.5% |
| Small/identity | 76,412 | 98,481 | +28.9% | −16.5% |
| Wide/identity | 22,537 | 27,779 | +23.3% | −26.6% |
| Large/identity | 10,225 | 14,068 | +37.6% | −20.2% |

Raw runs: [small/full](small-full.raw.json), [wide/full](wide-full.raw.json),
[large/full](large-full.raw.json), [small/identity](small-identity.raw.json),
[wide/identity](wide-identity.raw.json), [large/identity](large-identity.raw.json).
The fixtures stay within the common numeric/input domain. This comparison does
not test the new API's intentional overflow rejection as a throughput benefit.
The candidate also contains foundation changes; use the source overlay to
identify its exact scope rather than attributing every cycle to one commit.

## Strict append-view versus current identity receipts

Both sides use the same candidate library. The benchmark-only ctypes adapter
encodes native input through standard `json.dumps`, appends through the new
strict view function, reads sequence/time and releases the view. The baseline
uses the existing identity function. The consumer verifies complete native data
in both cases. No compiled Python view extension participates.

| Workload | View throughput change | Combined CPU change |
| --- | ---: | ---: |
| [Small](small-view.raw.json) | −15.1% | +13.0% |
| [Wide](wide-view.raw.json) | +1.8% | +14.9% |
| [Large](large-view.raw.json) | −4.9% | +4.5% |

The control changes both the writer implementation and result ownership. It
therefore exposes a regression to investigate, without isolating which part
causes it. The new Rust-per-value writer, owner normalization and extra ctypes
identity/release calls need separate budgets before adoption. Wide throughput
alone hides the CPU regression.

[Owner-forwarding runs](small-forward.raw.json) compare a prevalidated immutable
owner with ordinary encoded-byte append, using the same candidate library.
Small/wide/large throughput rises 2.4%/3.4%/2.1%; combined CPU changes
−0.8%/−24.7%/−25.6%. The [wide](wide-forward.raw.json) and
[large](large-forward.raw.json) cases remove repeated validation work, while
reader/storage work limits complete-path throughput. This intentionally compares
different ownership guarantees at the input boundary; it does not claim that an
arbitrary mutable buffer can skip validation.

## Core selected reads and snapshot-copy budget

Seven release processes alternate forward/reverse lane order. Each lane warms
before timing; cold means fresh owner/message acquisition, with warm storage
pages. Both cold selected lanes copy the complete snapshot. The JSON lane then
builds the native value tree; the view lane builds a validated owner and reads
one integer. [Raw timing runs](core-lanes.raw.json) retain every repeat.

| Fixture | Contiguous copy | Whole-root validation | Cold view + selected value | Cold JSON + selected value |
| --- | ---: | ---: | ---: | ---: |
| Small | 22 ns | 91 ns | 657 ns | 556 ns |
| Wide | 428 ns | 9.37 µs | 32.61 µs | 50.38 µs |
| Long string | 857 ns | 3.79 µs | 5.23 µs | 18.76 µs |
| 1,000-element array | 538 ns | 5.24 µs | 11.92 µs | 23.42 µs |

Cold view time rises 18.1% for the small record and falls 35.3%/72.1%/49.1% for
wide/string/array records. Warm view lookup takes about 64–67 ns. The copy probe
measures hot memory copying, without file locks, faults or contention; it does
not isolate the full ring-acquisition cost. Do not add independently measured
stage medians to reconstruct a complete operation.

Current cold owner construction also normalizes duplicate-key lookup state in
an extra pass. Array enumeration currently allocates a result vector; the
1,000-element case takes about 11.3 µs. These remain implementation costs,
not evidence of allocation-free sequential iteration.

The [separate allocator process](core-allocations.raw.json) reports Rust allocator
requests and deliberately emits null timings. C malloc and mapped storage do
not enter those counters. A retained child keeps the full encoded payload;
reported encoded byte length excludes owner/index metadata and is not RSS.

## Binding baseline and coverage

The [five-runtime baseline](five-runtime.raw.json) verifies Rust, C, Python,
Node and Go append/read calls over sensor, wide and large fixtures. It includes
three repeated alternating runtime orders with 200 records per case. C consumes
JSON input and returns JSON; Go's full read includes decoding RawMessage. These
are explicit result contracts, not a language ranking. The Node addon has its
own fingerprint and need not contain the same embedded build as the C library.

Python also has cold selected, warm retained-native-dictionary, array scan,
requested JSON and raw-forward controls. Unsupported new-view/native-byte jobs
remain explicit in the capability report. Future binding tasks must add and
measure their real API lanes rather than relabel these controls. The separate
[Go allocation run](go-allocations.raw.json) enables counters; its timings must
not enter timing comparisons. Session CPU includes caller/worker protocol work
and verification even though individual call latency excludes request IPC.

Paced delivery, fan-out and the JSON pipe controls remain in
`scripts/bench_pipe_comparison.py` and the existing wakeup evidence. This new
capture establishes saturated baselines and core costs; it adds no new paced
or multi-reader performance claim.

## Reproduce

Build outside the timing window. The benchmark script's help lists C/Go worker
build commands. Then use:

```sh
python3 scripts/bench_language_boundaries.py --inventory
python3 scripts/bench_language_boundaries.py --output baseline.json
python3 scripts/bench_language_boundaries.py --lanes go --diagnostics --output allocations.json
python3 scripts/bench_pipe_comparison.py --baseline-library BASELINE/target/release --baseline-bindings BASELINE/bindings/python --output full.json
python3 scripts/bench_pipe_comparison.py --receipt identity --baseline-receipt identity --baseline-library BASELINE/target/release --baseline-bindings BASELINE/bindings/python --output identity.json
python3 scripts/bench_pipe_comparison.py --receipt view --baseline-receipt identity --baseline-library target/release --output view.json
python3 scripts/bench_pipe_comparison.py --binary --forward-view --baseline-library target/release --output forward.json
target/release/examples/bench_owned_views 2000 --reverse
target/release/examples/bench_owned_view_allocations 200
```

Set `PLASMITE_BENCH_EXTRA_FIELDS=200` for wide fixtures or
`PLASMITE_BENCH_NOTE_BYTES=65536` for large fixtures. Use the counts and repeat
settings in each raw file. Alternate forward/reverse core processes, and keep
allocation probes, builds and other benchmarks out of timed runs.
