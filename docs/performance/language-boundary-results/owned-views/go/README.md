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

## Consumer tradeoffs

Prepared selectors and typed struct decoding cross the native boundary in coarse
calls. Sparse reads avoid decoding unused fields. Warm ordinary Go objects still
win repeated lookups after their initial construction. Full generic conversion
uses the JSON-compatible fallback and can lose to decoding cached JSON directly.

[Initial consumer runs](consumer.raw.json),
[partial-struct comparisons](dispatch-exact.raw.json), and
[stream results](streams/README.md) retain allocation, native-call, sizing and
complete-delivery controls. Children retain immutable buffer headers, so
reassigning a MessageView cannot change a child's data. This lifetime guarantee
adds metadata per selected child; the stream report measures that cost.

## Accepted array producer comparison

[Seven paired central runs](array-packing-runs/manifest.json) compare the fixed
original and compact-array Go executables with the **same release library**.
Each process runs 2,000 iterations per lane and prints PASS after fixture/output
checks. [The full summary](array-packing-runs/summary.json) retains all native and
JSON controls, allocation counts and cgo calls. Post-run loader probes confirm
the documented shared-library resolution; hashes identify unchanged artifacts.

| 1,000-integer fixture | Original native | Compact native | Candidate JSON |
| --- | ---: | ---: | ---: |
| Append, owned result | 79.09 µs | 35.08 µs | 46.44 µs |
| Append/read/typed decode | 90.70 µs | 45.93 µs | 59.12 µs |
| Go bytes per append | 195,454 | 58,620 | 53,831 |
| Go allocations per append | 18 | 12 | 18 |
| Go bytes per pipeline | 245,946 | 109,113 | 104,464 |
| Go allocations per pipeline | 24 | 18 | 24 |

Array append takes 56% less time and the complete pipeline takes 49% less. Native
now takes 24%/22% less time than the matching JSON append/pipeline. Crossing
counts stay at two per append and six per pipeline. The gain removes the large
per-element descriptor construction and growth copies; the boundary keeps one
compact value-at-visit snapshot. It changes no storage format or callback order.

Other producer medians retain the scope of the result:

| Append fixture | Original native → compact native | Candidate JSON |
| --- | ---: | ---: |
| Small map | 5.51 → 5.35 µs | 5.20 µs |
| Small struct | 5.37 → 4.95 µs | 4.94 µs |
| Wide | 44.19 → 44.16 µs | 39.47 µs |
| Long text | 13.77 → 14.08 µs | 54.31 µs |
| Native bytes | 16.61 → 17.12 µs | 57.53 µs |

Wide append remains 12% slower than JSON and allocates more Go bytes despite
fewer allocations. Wide pipelines rise 5.1% while their JSON control rises 3.9%.
The native-byte pipeline rises 5.1% while its JSON control stays near its earlier
time. The report retains those regressions; this array change supports no
universal producer speedup. Text/byte routes still beat the JSON controls
substantially, with their previously documented stored-kind distinction.

These fixtures use the existing subtree-copy forwarding path. They precede the
new stream whole-message forwarding shortcut and the separate retained-child
wrapper reassignment fix; their frozen executables never load later Go code.
The wide-object producer gap remains a limit of this implementation.

## Unified message check

On an Apple M5 Pro, seven runs of 5,000 messages compared the earlier Go view
path with `Fetch` and `Feed` using the unified message. Both paths used the same
debug native library. The benchmark opened a new finite reader every 128
messages. The sparse read decoded one integer into a struct. The forward sent
the same stored bytes to a second local pool and verified the result.

| Work per message | Earlier view median (range) | Unified message median (range) |
| --- | ---: | ---: |
| Small object, sparse read | 3.81 µs (3.71–4.70) | 3.81 µs (3.78–3.87) |
| 32,000-byte string, sparse read | 8.59 µs (8.50–9.56) | 8.70 µs (8.61–8.93) |
| Small object, read and forward | 6.38 µs (6.33–6.64) | 6.59 µs (6.47–6.73) |
| 32,000-byte string, read and forward | 13.21 µs (13.12–13.93) | 13.35 µs (12.90–13.98) |

The unified message adds 200 Go bytes to each read and 400 bytes to the
read-and-forward cycle, with the same cgo call count. A local forward shares
the source payload when its metadata holds only tags. Forwards that must
change metadata use the regular feed path.

After staging the debug library as in `just bindings-go-test`, run this command
from `bindings/go` with the same native library paths.

```bash
go test ./local -run '^$' \
  -bench '^BenchmarkOwnedStreams/(small|long)/(view_selected|message_selected|view_forward|message_forward)$' \
  -benchtime=5000x -count=7
```

## Reproduce

Use the benchmarks in [bindings/go/local](../../../../../bindings/go/local/):
`BenchmarkOwnedViews` and `BenchmarkNativeInput`, plus the local and UDP stream
benchmarks. Build against a fixed release library using the SDK's documented
linker settings. The raw manifests record iteration counts and environment.
Compare the same requested Go result, count cgo calls separately from timing,
and keep Go allocations distinct from native allocation counts.
