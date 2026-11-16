# Node views and input performance

Natural property access uses Proxy wrappers over immutable Lite3. Reading a
selected field avoids constructing unused JavaScript objects. Arrays support
iteration; explicit native conversion builds ordinary containers. A child
retains its entire message. See the [Node SDK](../../../../bindings/node/README.md)
for the public interface.

Native streams keep parked reads off Node's shared filesystem and DNS worker
pool. This improves delivery to a rare active reader among many idle readers,
while private workers and per-handle waits consume threads and memory. Tiny
message views and full conversion can cost more than JSON; the comparisons
below retain those limits.

## Reproduce

The runnable harnesses live in
[bindings/node/scripts](../../../../bindings/node/scripts/):
`bench_views.js`, `bench_input_views.js`, `bench_input_cold.js`,
`bench_stream_scheduling.js`, `bench_portable_views.js`, and
`bench_json_result_copy.js`. Each script documents its arguments. Select fixed
packages with `PLASMITE_BENCH_BINDING`, alternate before/after runs, and separate
cold startup, warmed loops, process CPU, and allocation diagnostics.

[Input summaries](input-batching-summary.json),
[scheduling summaries](stream-scheduling-summary.json), and
[copy comparisons](raw-vs-json-copy.raw.json) retain measurements and controls.
Encoding through the portable core lost enough complete-path comparisons that
it did not become the native input default; the
[rejected experiment](wasm-input-reuse-summary.json) retains cold and warm costs.

## Integrated scheduling checkpoint

A release addon from the integrated source passes 48 fresh-process runs: local
and UDP, one/four/32/128 handles, JSON and owned results, three alternating orders,
100 records per handle. Every delivered value and sequence check passes. The
[summary](integrated-scheduling-summary.json) and
raw runs, exact sources and hashes (outside Git) retain
the complete comparison. Both result paths use the same frozen core library.

These tiny records expose wrapper costs. At 128 local handles, JSON takes
131 ms elapsed and 303 ms process CPU; owned selected reads take 157 and 269 ms.
At 128 UDP handles, JSON takes 605 ms elapsed and 352 ms consumer CPU; owned
reads take 600 and 432 ms. UDP timing excludes the server's CPU. These results
support no claim that views always beat full-message JSON parsing.

Filesystem probes finish in about 0.7–0.9 ms while native reads wait. Pending
128-handle runs use 144 threads and roughly 20.5 MiB resident memory locally or
29.5 MiB for UDP. Thread samples and resident memory describe this harness,
including its zero-timeout probes and runtime setup. The earlier alternative
matrix isolates the scheduling decision and rare-active-reader latency; this
checkpoint verifies the combined implementation and owned-result costs.

## Shared input traversal and bounded native writes

JavaScript now observes input objects in one shared walker for native and portable
encoding. The native sink sends at most 512 primitive operations per call through
10 KiB of reusable staging storage. Native code feeds those operations to the
existing Lite3 builder. It creates no intermediate native object tree or encoded
message copy. Bytes and checked subtrees reach the builder at their visit; later
hooks cannot change them. A small record combines initialization, its final span
and commit in one native call. The obsolete Rust object walker has retired.

The [five-pair steady summary](input-batching-summary.json) uses 500 untimed
warmups per lane, 2,000 operations (500 for the array), three alternating lane
orders and natural garbage collection. Medians first combine repeats within a
process, then combine process medians. The summary retains cold observations
and portable controls. All measured payload and identity checks pass.

| Input | Append + field before/after µs | Append CPU before/after µs | Append/read + field before/after µs |
| --- | ---: | ---: | ---: |
| Small | 15.60 / 13.64 | 17.55 / 16.99 | 29.79 / 28.65 |
| Wide | 113.12 / 84.19 | 118.31 / 89.24 | 139.03 / 112.55 |
| 64 KiB string | 27.44 / 25.81 | 32.48 / 31.12 | 44.90 / 41.80 |
| 4,096 objects | 1,214.81 / 530.14 | 1,263.21 / 573.37 | 1,390.47 / 693.72 |

The array saves about 56% of append time and 50% of producer/consumer time.
Its producer/consumer CPU falls from 1,474 to 777 µs. JSON producer controls stay
flat. JSON input still wins wide and object-array construction on these JSON-safe
fixtures; the string favors native input. Exact integers and native bytes have
their own checks and cannot use lossy JSON substitutions as equivalent controls.

Short runs tell a different story. With 20 warmups, 200 operations and forced GC
before each timed row, the one-call candidate's small append takes 21.90 versus
20.45 µs, and some CPU rows lose despite lower elapsed time. Longer steady runs
change warmup, duration and GC policy together. They establish that workload;
they do not isolate which VM stage causes each short-run difference.

Fresh-process first append takes about 1.35 versus 1.15 ms for small input and
1.55 versus 1.25 ms for wide input. The first array takes 2.38 versus 2.61 ms.
Cold costs include runtime initialization and first use; they remain separate
from steady throughput. Memory observations include runtime allocations and do
not establish exact peaks. The sink clears string references and retains one
10 KiB idle packet; reentrant captures use independent packets.

Actual portable views now import correctly into native pools. The bridge checks
the configured NativeView constructor and caches one checked native import per
foreign backend. A portable owner no longer passes as a native owner. Tests cover
whole/root/mixed/retagged inputs with exact integers, bytes and retained children.
Resource and encoded-size errors can surface at a flush or finish; deterministic
type/key/depth checks keep visit order, and failures publish no partial message.

## Reuse owned completion for UDP DATA

Owned UDP DATA now uses the local owned-message completion case. This removes an
intermediate Rust allocation and a second identity event that JavaScript
discarded. Legacy DATA and control events retain their existing representation.

[Five paired comparisons](udp-owned-completion-summary.json), with
raw runs and source (outside Git), show one-handle
saturated consumer CPU at 7.44 versus 6.31 ms per 100 messages. At 128 handles,
elapsed time changes from 558 to 591 ms and consumer CPU from 433 to 474 ms.
The before CPU range spans 321–627 ms; the unchanged JSON control also varies
widely. The one-handle paced p95 median rises from 317 to 372 µs. Keep these higher
median costs visible. This simplification establishes no general fanout performance gain. The benchmark
measures the complete producer/consumer process and excludes the UDP server CPU.
