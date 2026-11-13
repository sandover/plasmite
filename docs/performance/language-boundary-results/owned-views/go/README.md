# Go-managed owned views

This checkpoint keeps immutable Lite3 snapshots in the Go heap and sends their
bytes and offset-only proof to native code only during synchronous calls. No
ordinary message needs `Close`, a finalizer, or a native memory owner. Go allocates
four-byte-aligned storage, C copies under the pool lock, then validation runs
after the binding releases that lock. Detached exports own their memory.

Prepared selectors fill pointer-free scalar and span slots in one cgo call.
Strings receive one final Go copy on extraction. Ordinary nested structs use a
cached field plan and direct final assignment. Complete exact shapes use keyed
lookups; partial ASCII shapes scan each selected object once and skip unrequested
subtrees. Unicode names, distinct case aliases, custom hooks, embedding, pointers
and type/range/null cases use `encoding/json`. Missing fields preserve the
caller's existing values. Every fast assignment passes its checks before any
field changes, so fallback keeps the standard decoder's error/update behavior.

Small C wrappers return metadata by value. This avoids separate escaping Go
output-pointer variables. Normal acquisition needs two Go allocations: owner and
encoded buffer. Empty duplicate proof needs no native allocation or free call.
Duplicate objects retain only offset metadata in Go; canonical lookup, count and
JSON tests agree on the last native-order winner.

## First consumer measurements

Fresh means a new owner from a resident pool; it does not mean cold storage.
Seven processes alternate fixture and lane order, with 2,000 operations per lane,
`GOMAXPROCS=1` and `GOGC=100`. All lanes use the same release library and Go test
binary. Native-call counters and Go allocation counts sit outside timing;
allocations below count Go memory. C snapshot controls also allocate a temporary
Rust payload. Full-result checks include the 1,000-element array sum.

| Complete operation | View ns/op | JSON control ns/op |
| --- | ---: | ---: |
| Fresh small typed struct | 1,065 | 1,672 |
| Fresh 64 KiB string typed struct | 12,578 | 66,263 |
| Fresh sparse fields from wide object | 6,518 | 29,449 |
| Fresh sparse fields from array fixture | 11,368 | 28,639 |
| Warm small typed struct | 215 | 248 |
| Warm 64 KiB string typed struct | 3,205 | 41,705 |

The JSON control reads directly into the requested struct; it does not first
build an entire native object tree. Sparse view reads request sensor and power;
the JSON control also decodes zero. Warm selective reads take 132–150 ns for
small/wide/array fixtures; already decoded native-object lookups take 18–20 ns.
These warm controls exclude initial native decoding. The long-string native
control compares the existing string and allocates nothing; view extraction
creates a new independent string.

The initial complete-shape plan had material limits. Partial typed structs with
extra fields fell back: fresh wide reads take 32.9 us versus 29.8 us for JSON; array reads take
33.1 versus 28.5 us. Warm full native-map conversion takes 846 versus 598 ns for
small data, 44.9 versus 36.2 us wide, 60.3 versus 40.4 us for long strings, and
82.9 versus 71.1 us for arrays. Views must format JSON before Go's full native
parser; the raw JSON control already holds formatted bytes. Keep those limits
visible when choosing an explicit conversion path.

## Sizing and copy controls

| Fresh owner | Small ns/op | Small Go bytes/op | Go allocations/op | cgo calls/op |
| --- | ---: | ---: | ---: | ---: |
| Read into initial 1 KiB Go buffer | 844 | 1,104 | 2 | 2 |
| Query exact size, then read into Go buffer | 1,292 | 656 | 2 | 3 |
| Native snapshot, Go copy, then same validation | 869 | 760 | 5 | 3 |

The exact size probe saves 448 Go bytes but adds 53% small-read time. The default
keeps the initial 1 KiB buffer. Its slack remains live with small views; native
copies retain the smaller final payload after their temporary objects die.
Wide read-into takes 6,293 ns versus 5,854 ns for native-copy control; long strings
8,917 versus 9,351 ns; arrays 11,009 versus 11,250 ns. These results support fewer
crossings and allocations, without a universal copy-speed claim. Larger records
need a capacity retry; its first 1 KiB allocation adds one temporary Go allocation.
No retry consumes a stream item; these measurements use point reads.

## Reproduction

[Raw runs](consumer.raw.json) contain every lane, process CPU, source hashes,
artifact hashes and toolchain metadata. Exact source (outside Git)
contains the compiled Rust/C/Go source, including then-untracked registered
modules. The measured fixtures contain no duplicate keys. A later focused test
caught an over-strict length guard on nonempty duplicate proof; current source
removes it and passes Rust/Go duplicate-boundary tests plus `cgocheck2`. The empty
proof path in these measured lanes never executes that corrected branch.
Vendor sources come from the recorded base commit. The benchmark lives
in `bindings/go/local/views_bench_test.go`.

Build a release core and run the Go test compiler with the local header/library
overrides from `bindings/go/README.md`, then run the saved test executable with
`-test.run=^$ -test.bench=BenchmarkOwnedViews -test.benchtime=2000x -test.count=1`.
Alternate `PLASMITE_GO_REVERSE=0/1` and keep its dynamic-library path fixed.
Focused correctness and race tests cover capacity retry, concurrent reads,
retained children after close, detached exports, exact integers, negative zero
and parity with standard JSON fallback.

## Partial structs and exact-shape recovery

The native dispatcher accepts unambiguous ASCII struct field names. It borrows
the encoded key spans, matches ASCII case without allocating keys, and fills
only requested scalar/string slots. It visits selected objects once and leaves
unknown arrays and nested objects untouched. Distinct record case aliases defer
to Go's decoder: earlier aliases can merge objects or produce errors even when
a later alias has a valid value. Equal native duplicate keys keep their
canonical last occurrence.

Complete exact objects retain the cheaper point-lookup path. The encoded object
already stores its count; when count and all original field names match, that
proves the absence of extra aliases. A failed proof clears temporary matches
before scanning. This needs no second host cache and no additional crossing.
Scalar leaves now read their native value once.

| Typed decode | Original frontend ns/op | Final dispatcher ns/op |
| --- | ---: | ---: |
| Fresh small complete struct | 1,162 | 1,109 |
| Warm small complete struct | 218 | 211 |
| Fresh partial struct, 200 extra ASCII fields | 24,647 | 9,843 |
| Warm partial struct, 200 extra ASCII fields | 18,299 | 4,082 |
| Fresh partial struct, unrequested 1,000-value array | 33,579 | 11,895 |
| Warm partial struct, unrequested 1,000-value array | 20,999 | 255 |

The final JSON controls take 21.8 us for fresh ASCII-wide typed reads and 29.9 us
for the array fixture. Unicode-wide fallback stays near 34 us; long-string typed
reads move about 2%, alongside similar control movement. Warm exact sparse reads
stay near 140–153 ns. These results support the dispatcher for ordinary partial
structs while retaining the complete-shape path.

The first scan-only implementation regressed small complete reads by 6% fresh
and 18% warm. [Its paired runs](dispatch.raw.json) retain that result; the final
[exact-shape runs](dispatch-exact.raw.json) show the recovery. Both runs use seven
alternating process pairs, the same updated benchmark code in both Go variants,
and one frozen core library per comparison. Sparse JSON controls now decode the
same two fields as the selector. The final comparison uses library `9fb9a136` and
its verified complete source archive from the batch build; later workspace
source changes never enter these private artifacts.

Baseline source (outside Git) and
candidate source (outside Git) combine the exact core
archive with each compiled Go frontend. The baseline uses `4e7fa95` consumer code
and the updated shared benchmark. Raw metadata records every file/artifact hash.
All final ordinary/duplicate/alias cases, race checks and strict `cgocheck2`
checks pass. A separate forced-GC diagnostic retains 10,000 small views: the
initial buffer retains about 1,100 Go heap bytes per owner, versus 656 for exact
sizing/native-copy controls. All retain two Go objects per owner; initial-buffer
slack remains the deliberate small-read latency tradeoff.


## Native producer checkpoint

`AppendView` collects ordinary Go values into pinned native descriptors, writes
Lite3 directly, and returns a Go-owned committed snapshot. Immutable strings and
sealed views borrow their storage for one synchronous call. Mutable byte slices
and custom JSON results take a snapshot when visited, so later hooks cannot
change an earlier value. Custom hooks run outside the pool mutex. Embedded fields,
map-key text methods, omission rules and quoted scalar tags follow Go's JSON
field rules; native byte slices retain Lite3's byte type.

[The checkpoint manifest](producer-checkpoint.json) records the release library,
Go test executable and every compiled source hash. The complete source
overlay (outside Git) includes registered Rust modules and the Go
producer/consumer harnesses. Apply it over the recorded base commit, which
supplies vendored dependencies. The fixed executable and library remain at
`tmp/go-input-checkpoint/input.test` and `tmp/go-input-checkpoint/lib` for the
central measurement run. A later Go-only omission-rule test adds coverage; it
does not change the compiled runtime or benchmark source.

Run the saved executable with the fixed library directory on the dynamic linker
path and `-test.run=^$ -test.bench=BenchmarkNativeInput -test.benchtime=2000x
-test.count=1`. Alternate `PLASMITE_GO_REVERSE=0/1`. The harness covers small maps
and structs, wide objects, 64 KiB text/bytes and arrays. Each fixture compares
owned native/JSON append results, forwarding and append/read/typed-decode
pipelines, checks the final values and reports allocation/cgo-call counts. Byte
lanes compare the same decoded byte output while storing native bytes versus
JSON base64 strings. This checkpoint makes no producer performance claim; the
full matrix belongs to the central run.

Focused tests cover mutable-byte and reusable-hook buffers, map snapshots,
reentrant close, exact integers, finite binary64 values, negative zero, native
bytes, retained owners, embedded-field dominance, custom map keys, quoted tags,
absolute depth through source/JSON subtrees and invalid text normalization.
The full local/API race suite and strict cgo-pointer checks pass.


## Central producer results

[Seven complete runs](producer-runs/manifest.json) use the fixed checkpoint
executable/library above, 2,000 iterations per lane, and alternating fixture/lane
order. All seven print PASS. [The summary](producer-runs/summary.json) records
medians, cgo calls and Go heap allocations. Values below show native versus JSON
time in microseconds; both append routes return an independent Go-owned view.
Pipelines append, fetch and decode selected typed fields. They do not decode the
unrequested wide fields or large array.

| Fixture | Append native / JSON | Forward native / JSON | Pipeline native / JSON |
| --- | ---: | ---: | ---: |
| Small map | 6.11 / 5.68 | 5.32 / 5.47 | 6.75 / 7.02 |
| Small struct | 5.12 / 5.15 | 4.86 / 4.66 | 5.74 / 5.87 |
| 200 extra fields | 46.08 / 42.85 | 13.59 / 18.96 | 53.76 / 54.01 |
| 64 KiB text | 15.90 / 56.15 | 12.81 / 48.26 | 30.07 / 71.90 |
| 64 KiB bytes | 18.50 / 59.34 | 12.65 / 75.36 | 30.22 / 172.93 |
| 1,000 integers | 85.12 / 48.57 | 43.49 / 49.83 | 94.06 / 62.18 |

Native append removes text formatting/parsing and takes 72% less time for long
text, 69% less for bytes. Byte lanes preserve the same decoded byte output,
while the native record stores bytes and the JSON record stores a base64 string.
The byte pipeline takes 83% less time and uses six cgo calls versus eight.
Forwarding long text and bytes takes 73% and 83% less time; wide forwarding takes
28% less time. Ordinary native/JSON append both use two cgo calls. Native
forwarding uses two versus four because it avoids the explicit JSON export.

The array producer remains a material regression: native append takes 75% more
time and the pipeline takes 51% more. Wide/small-map append also takes about 8%
more; small structs and wide pipelines stay near their JSON controls. No
universal native-producer speedup follows from these results.

| Append fixture | Native Go bytes / allocations | JSON Go bytes / allocations |
| --- | ---: | ---: |
| Small map | 1,212 / 10 | 810 / 15 |
| Small struct | 1,012 / 6 | 709 / 6 |
| Wide | 62,829 / 22 | 20,829 / 615 |
| Long text | 74,461 / 10 | 149,237 / 15 |
| Bytes | 140,541 / 12 | 183,729 / 18 |
| Array | 195,453 / 18 | 53,812 / 18 |

These are Go heap counts; they exclude native writer allocations. Fewer
allocations alone do not establish a faster producer. The native collector
creates one 64-byte tagged descriptor per scalar/container before writing Lite3.
A thousand small integers therefore expand into at least 64 KiB of descriptors,
plus growing backing arrays, while JSON writes compact integer text. The array
lane allocates 3.6 times as many Go bytes despite identical allocation counts.
This source accounting identifies the temporary descriptor expansion; a targeted
follow-up should remove it before attributing the remaining time to reflection,
pinning or the native writer. The consumer and native-byte functionality pass;
the array producer performance gap remains open.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
