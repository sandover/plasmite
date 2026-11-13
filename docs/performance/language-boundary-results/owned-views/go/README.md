# Go-managed owned views

This checkpoint keeps immutable Lite3 snapshots in the Go heap and sends their
bytes and offset-only proof to native code only during synchronous calls. No
ordinary message needs `Close`, a finalizer, or a native memory owner. Go allocates
four-byte-aligned storage, C copies under the pool lock, then validation runs
after the binding releases that lock. Detached exports own their memory.

Prepared selectors fill pointer-free scalar and span slots in one cgo call.
Strings receive one final Go copy on extraction. Ordinary nested structs use a
cached field plan and direct final assignment when the complete exact shape
matches. Custom hooks, embedding, pointers, missing/null/type/range cases and
extra fields use `encoding/json`. The initial complete-shape rule avoids changing
Go's case-insensitive field precedence. A later dispatcher can widen that fast
case without changing the API.

Small C wrappers return metadata by value. This avoids separate escaping Go
output-pointer variables. Normal acquisition needs two Go allocations: owner and
encoded buffer. Empty duplicate proof needs no native allocation or free call.
Duplicate objects retain only offset metadata in Go; canonical lookup, count and
JSON tests agree on the last native-order winner.

## Matched release measurements

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

This checkpoint has material limits. Partial typed structs with extra fields
fall back: fresh wide reads take 32.9 us versus 29.8 us for JSON; array reads take
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

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
