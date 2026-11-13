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

## Follow-up: traverse parsed JSON in C

The next candidate replaces the strict JSON writer's Rust-per-value traversal
with a C traversal of the already parsed document. It avoids per-key CString
construction, temporary Rust entry vectors and repeated per-value foreign calls.
The same ctypes view adapter and complete native-data consumer run on both
libraries, so result contracts match. Seven alternating repeats use the same
counts/fixtures as the first capture. The legacy full-result path acts as a
control. No builds/tests ran during the timing window.

| Workload | Previous view messages/s | C traversal messages/s | Throughput change | Combined CPU change |
| --- | ---: | ---: | ---: | ---: |
| [Small](c-traversal-small-view.raw.json) | 83,503 | 89,658 | +7.4% | −5.0% |
| [Wide](c-traversal-wide-view.raw.json) | 28,323 | 27,878 | −1.6% | −7.5% |
| [Large](c-traversal-large-view.raw.json) | 13,505 | 13,620 | +0.9% | −0.4% |

The wide writer's median CPU falls from 0.172880 to 0.144757 seconds (−16.3%),
while reader CPU changes from 0.174727 to 0.175933 seconds (+0.7%). Removing
writer work does not establish a faster complete pipeline; throughput regresses
1.6% in this capture. The remaining reader/storage/scheduling costs need their
own attribution before making a stronger claim.

Legacy full-result throughput changes 0.0%/+0.7%/0.0% for
[small](c-traversal-small-full.raw.json), [wide](c-traversal-wide-full.raw.json)
and [large](c-traversal-large-full.raw.json), with combined CPU within 0.6%.
The first capture's view-versus-identity gap remains a separate result: this
follow-up improves the new writer but does not make view ownership free.

[C-traversal environment/source fingerprints](c-traversal-environment.json)
identify both libraries, the unchanged benchmark/bindings, build command and
source overlay. The candidate also contains unused bounded-cursor API additions
and test-only changes; owner acquisition/normalization did not change. The
candidate source overlay (outside Git) preserves this exact
intermediate build over the baseline checkout. The initial saved library's
SHA-256 remains `9a2f9478f827d610253291adb207cffab999667387c4ef8f41e97e36906c7934`;
the new library is
`701f655d9ad28e6b7ebc54649069d1e87e78c92638230a0e76f40be79fa97c45`.

Reproduce the strict-view control with `--receipt view --baseline-receipt view`
and the saved prior library directory. Use `--receipt full --baseline-receipt full`
for the legacy control. The [summary](c-traversal-summary.json) retains medians;
the raw files retain every repeat and per-worker verification/CPU.

## Bounded sequential cursor

The C cursor retains one immutable owner and a fixed 256-byte private iterator
state. It fills caller-sized chunks. Rust keeps that state inline; its borrowed
iterator allocates no entry vector. Existing whole-collection enumeration remains
available as the control. The shim derives key lengths from checked entry
boundaries because the pinned native iterator reports the encoded tag length
without its two-bit shift.

Seven release processes alternate forward/reverse lane order and verify every
array sum. Separate allocator diagnostics count Rust allocations, not C malloc.
The timing executable uses the normal allocator. Both lanes use the same fixture
and executable; setup and validation sit outside these warm traversal timings.

| Warm operation | Median ns/op | Rust allocations/op | Requested Rust bytes/op |
| --- | ---: | ---: | ---: |
| Enumerate 1,000 values through temporary entry vectors | 10,888 | 10 | 89,408 |
| Sum 1,000 values through the bounded cursor | 9,962 | 0 | 0 |
| Sum an already decoded Rust JSON-value array | 332 | 0 | 0 |
| Read one retained small-view field | 70.3 | 2 | 11 |
| Read the field from an already decoded small native object | 3.9 | 0 | 0 |

The cursor reduces this array traversal time about 8.5% and removes the temporary
entry allocations. Native objects remain much faster for repeated warm reads;
these measurements do not include their initial decoding cost. The view-field
allocation diagnostic also identifies remaining `data`/`power` C-string work.
Keeping a child still retains the complete encoded owner; this change does not
alter that lifetime tradeoff.

[Raw runs and fingerprints](cursor.raw.json) include process CPU, binary hashes,
source hashes and the separate allocation probe. Exact source (outside Git)
records the measured runtime and example files; unchanged vendor sources come
from the recorded base commit. Build with
`cargo build --release --example bench_owned_views --example bench_owned_view_allocations`,
then run `target/release/examples/bench_owned_views 2000`, alternating `--reverse`.
Use `target/release/examples/bench_owned_view_allocations 100` only for diagnostics.

## Length-aware lookup

Owned-view lookup now passes a borrowed UTF-8 span to the shim. Keys below 256
bytes receive a temporary stack terminator; longer keys use a checked temporary
allocation. NUL still means absent. The change removes the redundant Rust
CString construction for `data` and the requested member, while retaining the
same native lookup and local checks.

Seven paired release runs alternate executable order and lane order. The
preserved baseline executable matches the cursor checkpoint above; the candidate
also includes unused native-builder entry points, so native-object and legacy
JSON controls remain visible. Separate allocation diagnostics show two Rust
allocations and eleven requested bytes per warm field read falling to zero.

| Fixture | Baseline warm view ns/op | Span warm view ns/op |
| --- | ---: | ---: |
| Small | 64.5 | 34.8 |
| Wide | 66.8 | 43.3 |
| Long string | 63.1 | 34.9 |
| Array | 62.8 | 36.6 |

The small cold selected read falls from 609 to 551 ns; its native JSON control
moves from 549 to 538 ns. Large cold reads change about 1% and do not establish
a broad cold-path gain. Warm native-object controls remain faster; the exact
controls and process CPU appear in [raw paired runs](span-lookup.raw.json).
Candidate source (outside Git) and the preceding cursor source
archive preserve the two measured states. The candidate archive includes the
unused builder ABI additions. Long-key and empty/Unicode-key checks exercise
both temporary-storage paths; the benchmark's short keys need no C allocation.

## Python native container interface

The [Python extension measurements](python/README.md) cover fresh selected reads,
full native conversion, warm container slots, bounded array iteration, JSON
formatting, explicit ctypes fallback costs and separate allocation diagnostics.

## Single-pass owned-view validation

Fresh owners now check the complete tree, native key indexes, UTF-8, depth and
canonical message shape in one native traversal. The traversal reports only
objects with duplicate keys. Ordinary objects need no Rust entry vectors or
per-object hash maps. Duplicate objects retain their last-value correction map;
malformed indexes fail acquisition. Legacy byte validation keeps its existing
behavior. Valid native hash collisions and forged-index tests cover this split.

Seven paired release runs alternate executable and lane order. Separate
allocation diagnostics count Rust allocations; normal-allocator executables
supply timings. The candidate includes unused native-builder entry points;
legacy JSON and warm lookup controls expose changes outside acquisition.

| Fresh selected read | Before ns/op | Single pass ns/op | Rust allocations before / after |
| --- | ---: | ---: | ---: |
| Small | 545.1 | 271.9 | 12 / 2 |
| Wide | 32,323.9 | 15,028.8 | 418 / 2 |
| Long string | 5,122.1 | 4,765.9 | 12 / 2 |
| Array | 11,586.9 | 7,415.4 | 12 / 2 |

Warm lookup stays near 37–43 ns. Legacy JSON controls move from 542.9 to 562.2 ns
for small messages, 50,696.8 to 50,661.5 ns for wide messages, 18,207.9 to
18,847.9 ns for long strings, and 23,061 to 23,333 ns for arrays. The small and
long-string controls regress about 3.5%; these runs do not isolate the cause.
The acquisition gains come with complete index checks and preserve snapshot
ownership, pool overwrite safety and retained child lifetimes.

[Paired raw runs](strict-proof.raw.json) record CPU time, source and binary
fingerprints, checksums and allocator diagnostics. Exact candidate source (outside Git)
and the preceding span-lookup source archive preserve both measured states.
Use the same release examples and alternating `--reverse` command above.

## Cached JSON-native capability

The same strict walk now records whether every integer fits the exact binary64
integer range and the owner contains no native byte values. This conservative
whole-owner bit lets a binding choose its optimized JSON parser for explicit
native conversion. Private builder owners report false until their builders
track the property. Acquisition still applies the same validation rules.

Seven alternating paired runs compare against the exact strict-proof candidate.
Fresh small reads stay at 280 ns; wide reads move from 16,048 to 15,608 ns,
arrays from 7,601 to 7,487 ns, and long strings from 4,800 to 4,910 ns. The legacy
long-string JSON control also rises 4.5%. These results establish no acquisition
speed gain or broad regression. The cached field adds eight requested bytes per
owner through structure padding, with the same two allocations and no extra
walk. Warm lookup allocation counts remain zero.

[Raw runs](json-capability.raw.json) include all controls, separate allocator
probes and source hashes. Candidate source (outside Git)
includes an unused stored-order formatter and ABI exports; these acquisition
lanes never call them. The strict-proof source archive supplies the baseline.
The Node conversion measurements cover the consumer gain separately.

## Follow-up: build directly from native values

The new C builder lets a language binding write ordinary values into one growing
Lite3 allocation. It copies each input string during the call and keeps no caller
pointers. Appending returns an independent immutable view over that allocation.
It avoids constructing JSON text and parsing that text back into values.

The [matched C benchmark](native-builder.raw.json) compares this route with
`snprintf` JSON encoding followed by `plsm_pool_append_view_json`. Both routes
commit the same values, return the same retained view, and verify every value
and sequence. Seven alternating pairs run after 100 warmups, with no concurrent
builds or tests. The source archive (outside Git), runner,
worker and library fingerprints identify the measured build.

| Fixture | JSON producer | Direct producer | Complete JSON path | Complete direct path |
| --- | ---: | ---: | ---: | ---: |
| Small | 5.31 µs | 5.18 µs | 5.67 µs | 5.54 µs |
| 200 extra fields | 25.28 µs | 21.47 µs | 34.91 µs | 31.47 µs |
| 64 KiB string | 26.42 µs | 14.05 µs | 28.02 µs | 15.69 µs |

The direct producer saves 15% for wide objects and 47% for the long string.
The small difference changes sign between exploratory and final runs, so we
claim no small-message improvement. Complete process CPU per record, including
verification, falls from 4.56 to 4.52 µs, 31.62 to 28.17 µs, and 27.68 to
14.71 µs respectively. Per-operation wall timing includes the same two clock
reads on both sides; CPU timing uses two whole-run clock reads. We do not measure
allocation counts in this run.

These results measure C input conversion, not Python or JavaScript encoding.
Each binding must benchmark its own native encoder before choosing a default.
The controlled JSON fixture uses ASCII strings and finite exact numbers; its
`snprintf` control does not implement a general-purpose JSON encoder.

Reproduce after building outside the measurement window:

```sh
cc -O3 -Iinclude scripts/bench_builder_c.c -Ltarget/release -lplasmite -Wl,-rpath,$PWD/target/release -o target/release/bench_builder_c
python3 scripts/bench_builder.py --repeats 7 --output native-builder.raw.json
```

## Follow-up: decode Rust types directly

`ValueRef::decode` builds the requested Serde type directly from Lite3. Text and
byte fields borrow the snapshot when their target type permits it. Array size
hints let the decoder allocate a requested vector once. Unknown fields skip
already validated values without traversing their descendants. Struct decoding
still scans keys; it does not perform an indexed projection.

[Seven alternating release processes](rust-decode.raw.json) compare equal typed
outputs, including every array element and long-string content. Cold routes
both copy a snapshot; the old route creates a generic native message first.
The JSON control formats the same warm immutable data and parses it into the
same struct. Fixture setup stays outside timing. Builds/tests stopped during
timing, and allocation probes ran separately. See the source archive (outside Git)
and recorded source/example fingerprints for the measured implementation.

| Fixture | Cold direct typed | Cold old typed | Warm direct typed | Warm JSON typed | Warm native-tree typed |
| --- | ---: | ---: | ---: | ---: | ---: |
| Small | 425 ns | 616 ns | 184 ns | 209 ns | 28 ns |
| Wide | 18.74 µs | 52.79 µs | 3.53 µs | 29.65 µs | 405 ns |
| Long string | 6.32 µs | 19.73 µs | 1.42 µs | 21.83 µs | 1.15 µs |
| 1,000-element array | 21.60 µs | 24.70 µs | 13.89 µs | 14.80 µs | 693 ns |

Cold direct decoding takes 31%/65%/68%/13% less time. Retained native-tree
controls remain faster, especially for array scans. The common full-output
checks enter all timings; comparing the long string itself costs time even
when decoding borrows it. Do not interpret these as isolated field-access costs.

Warm direct typed decoding requests one Rust allocation for the output vector:
24 bytes in the first three fixtures, 8,000 bytes in the array case. Cold direct
decoding requests three allocations versus 15/1,044/17/23 for the old route.
Counters exclude C malloc and mapped storage. Full generic native conversion
also has matched lanes in the raw file; it remains more expensive than a sparse
typed result because it must create every requested object and string.

Reproduce:

```sh
cargo build --release --example bench_owned_views --example bench_owned_view_allocations
python3 scripts/bench_rust_decode.py --output rust-decode.raw.json
```

Tests cover borrowed string/byte pointers, options, enum shapes, integer range
errors, and float/integer type errors. A compile-fail example prevents borrowed
text from escaping its owner.

## Follow-up: native Rust encoding, append results, and retained children

Rust can now encode a Serde input into one growing Lite3 allocation and retain
that encoding as the append result. Custom serializers run once across growth.
Explicit JSON input parses once. Existing validated views can forward unchanged
without re-encoding, or supply data for a new encoding with changed tags. A
checked subtree copy also serves native bindings without host scalar conversion.

`ValueRef::into_owned` retains a child by sharing its snapshot. The operation
requests no heap allocation or payload copy. Its retained memory includes the
whole snapshot. Ordinary borrowed traversal still clones no ownership handle.

The [consolidated raw report](rust-codec.raw.json) and
complete source overlay (outside Git) supersede the earlier Rust
capture for reproduction. This overlay includes every compiled Rust/C/header
file, including parallel Go work outside these measured routes. Source hashes
remain unchanged through the release build and archive. Seven release processes
alternate lane order; allocation probes run separately. No builds/tests run
during measurements. The older Rust archive omitted an unrelated, then-untracked
Go module needed by its crate registration; use this complete overlay instead.

| Native input fixture | Direct encoding | JSON encoding + parsing | Time change |
| --- | ---: | ---: | ---: |
| Small | 532 ns | 616 ns | −14% |
| Wide nested object | 42.01 µs | 34.30 µs | +22% |
| 64 KiB string | 7.42 µs | 34.75 µs | −79% |
| 1,000-element array | 33.37 µs | 41.57 µs | −20% |

The wide direct path remains slower and requests 1,220 Rust allocations versus
23 for the JSON route. Its generic map-key handling creates an owned key and
then a C-terminated copy for each entry. This identifies the next optimization;
the result does not support choosing direct encoding universally. Retagging a
wide view is also slower: 58.93 versus 49.37 µs. Long-string retagging improves
from 34.77 to 7.65 µs. Both routes preserve the complete data and replace tags.

| Typed input fixture | Direct encode + append | JSON encode + parse + append |
| --- | ---: | ---: |
| Small | 3.44 µs | 3.69 µs |
| 64 KiB string | 12.23 µs | 39.64 µs |
| 1,000-element array | 36.92 µs | 45.65 µs |

These are single-producer Fast appends that return the same retained view, with
explicit timestamps. Both paths commit through the same pool operation. Output
checks run before timing; each measured append verifies a nonzero identity.
No reader runs concurrently, so this does not establish delivery throughput.
The typed input contains `power`, `samples` and optional `note`; wide extra fields
enter only native-tree and retag lanes, not this projected typed input.

The report also repeats all cold/warm decoder controls. Retained native trees
remain faster for repeated field access and array scans. Full output comparison
enters decoder timings; encoder lanes verify fixture outputs before timing.

Reproduce with the same command as the Rust decoder capture, using
`--output rust-codec.raw.json`. Tests additionally cover native byte values,
map-key types, custom serializer growth, native append/forward identity and
lifetime, retagging, and pre-publication rejection.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
