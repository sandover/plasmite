# Language interfaces and performance

Owned views help when an application reads a few fields, retains encoded data
or forwards a message. They keep Lite3 and its validation result instead of
rebuilding a whole native object tree at each language boundary. An application
that needs every field as ordinary language objects still pays for that copy.
Use `feed_view`, `follow_view` and `fetch_view` where the binding offers them.
They share options with `feed`, `follow` and `fetch`.

## Where the work went

A local read copies a record into an independent immutable snapshot and validates
it once. Children retain that snapshot. Checked writers establish the same
invariants while encoding, and unchanged forwarding reuses the bytes and proof
with a fresh destination identity. Pool storage and transports still copy bytes;
“zero-copy JSON” does not describe the complete path.

Each runtime needs its own efficient boundary. Python's native extension uses
container and buffer slots for Mapping/Sequence access. Go keeps immutable bytes
in its own heap and uses coarse native calls for selection and typed decoding.
Its homogeneous scalar arrays cross as compact spans, rather than one general
value descriptor per element. Mutable input snapshots preserve values at the
point the encoder visits them. These changes remove intermediate work without
changing the stored format or rerunning user hooks.

Explicit batches amortize stream locks, calls and result handling. Python's
`next_batch` waits for one accepted record, then takes available records within
count and encoded-byte bounds. It does not wait to fill the batch. Ordinary
iteration stays one record at a time; batching requires an application choice.

Node keeps waiting off the runtime's shared filesystem/DNS workers. Local streams
check for ready records on four private workers and park empty reads per handle;
UDP waits directly on its socket. This keeps an idle reader from delaying a busy
one. In the 128-reader alternative comparison, the rare active local reader's
median wait falls from about 54 ms to 0.14 ms. The
[integrated scheduling results](language-boundary-results/node-views/README.md#integrated-scheduling-checkpoint)
also record the thread/memory cost and slower tiny-message view cases.

## Matched results

Each row compares the same requested result within one recorded experiment.
The fixtures, runtimes and paths differ across rows; this table does not rank
languages. Times and throughput include the work named in the second column.
The linked reports preserve fixed artifacts, verification, raw runs and controls.

| Change | Matched work | Before → after | CPU or allocation context |
| --- | --- | ---: | --- |
| [Node shared input traversal](language-boundary-results/node-views/README.md#shared-input-traversal-and-bounded-native-writes) | 4,096 objects: append, read, selected field | 1,390 → 694 µs | Same-process producer/consumer CPU: 1,474 → 777 µs; first-call and short-run costs remain separate |
| [Go compact scalar spans](language-boundary-results/owned-views/go/README.md#accepted-array-producer-comparison) | 1,000 integers: append, read, typed decode | 90.70 → 45.93 µs | 24 → 18 Go allocations; 245,946 → 109,113 Go bytes per pipeline |
| [Python explicit batches](language-boundary-results/owned-views/python/batches/README.md#final-central-confirmation) | Complete message JSON/dataclass result, count 16 | 70,945 → 120,865 messages/s | Combined producer/reader CPU: 87.407 → 71.957 ms per 5,000 messages |
| [Python explicit batches](language-boundary-results/owned-views/python/batches/README.md#final-central-confirmation) | Native data result, count 16 | 102,241 → 134,932 messages/s | Combined producer/reader CPU: 67.997 → 60.480 ms per 5,000 messages |
| [HTTP retained append owner](language-boundary-results/http-views/README.md#removing-repeated-acquisition) | 4,096 objects: encode, append, receipt, selected field | 1,922 → 1,698 µs | Combined client/server CPU: 3,433 → 3,232 µs |
| [HTTP retained forwarding owner](language-boundary-results/http-views/README.md#removing-repeated-acquisition) | Forward the same object array | 883 → 693 µs | Transport still sends an independent body buffer |

The Go change removes descriptor expansion and growth copies; it keeps one
compact input snapshot. Python batches use a 256 KiB byte bound and the native
extension, with matching producer and conversion work. HTTP retains the owner
that append already built, eliminating a second copy/validation after the
receipt. Small HTTP append changes overlap run variation.

## Choose by the result your application needs

Sparse access can avoid decoding large unused values. In the
[portable Node experiment](language-boundary-results/portable-views/README.md#central-results),
a cold selected read of a record with a 64 KiB string took 8.58 µs through the
WebAssembly view backend, versus 16.46 µs for JSON parsing and the same access.
Portable small, wide and integer-array selected reads lost to JSON. Full native
copies lost on every measured fixture, including that string: 44.37 versus
16.09 µs. Warm loops over an already-native array also cost far less than
repeated view crossings. Use a native copy when repeated full-object work
outweighs the conversion you would save.

Forwarding an unchanged message avoids materializing its data; changing tags
rebuilds its envelope, and passing a subtree copies it into a new encoding.
[Go stream results](language-boundary-results/owned-views/go/streams/README.md)
include selected-read gains and material exceptions: full array conversion
regressed against JSON, and wide whole-message forwarding lost to raw binary.
[Python results](language-boundary-results/owned-views/python/README.md) separate
native-extension and ctypes fallback costs. [Node results](language-boundary-results/node-views/README.md)
identify each measured implementation; use their recorded build rather than
assuming later adapter changes have the same cost. [Rust writer and decoder
results](language-boundary-results/owned-views/README.md) cover direct Serde
encoding/decoding and retained construction proof.

A retained child keeps the complete message alive. Explicit mutable exports
copy; Python compatibility buffers also require extra encoded copies. Larger
batches retain more messages at once. Lite3 can occupy more bytes than compact
JSON, so fewer conversions do not guarantee less wire traffic or CPU. HTTP JSON and binary readers both report terminal errors. UDP views deliver
messages in arrival order, including duplicates and late packets.
See the [migration guide](../record/upgrading-2.0.md#keep-encoded-data-with-owned-views)
for exact-number rules, copy boundaries, cleanup and transport support.

## Plasmite 2.0 read and write check

On the same Apple Silicon Mac, release builds from `80a92f1` and the 2.0
candidate each read 10,000 retained small messages per run. The baseline
used the old reader with a record limit. The candidate used `fetch`. Each
run verified the same result. Seven alternating runs gave these medians
and ranges in nanoseconds per message.

| Representation | Before | 2.0 |
| --- | ---: | ---: |
| Encoded Lite3 | 788 (760–872) | 798 (760–820) |
| Decoded message | 1,377 (1,303–1,464) | 1,199 (1,096–1,278) |
| View, one field | 909 (880–973) | 946 (906–982) |
| View, full decode | 1,146 (1,110–1,216) | 1,175 (1,111–1,248) |

Five alternating 2,000-operation runs of `bench_owned_views` also compared
the old append calls with `feed_view`. Typed feed measured 3.47 → 3.68 µs
and JSON-to-view feed 3.78 → 3.97 µs. The ranges overlapped. An independent
cold field read measured 282 → 286 ns. The 2.0 feed samples the clock as
part of each write.

Build with `cargo build --release --example bench_view_streams --example bench_owned_views`.
Run `bench_view_streams local LANE small 10000` for `raw`, `json`,
`view_selected` and `view_full`, and `bench_owned_views 2000` for the
typed-input and sparse-view lanes. These checks cover local Rust paths.

### Indexed starts and HTTP streaming

The 2.0 performance fixes restore the sequence index for range starts and use
the same frame-header scan as point reads when the index misses. HTTP waits for
room in the output queue and formats JSON directly from the encoded frame.
These changes keep the existing selection and stream-ending rules.

Optimized builds on the same Apple Silicon Mac gave these medians against
`80a92f1`. The indexed pool had 300,000 small records and 600,000 index slots.
The HTTP transfers used loopback and read 2,000 records with a 16,000-byte
text field. Each run verified the requested sequences or returned record.

| Workload | 1.x baseline | 2.0 with fixes |
| --- | ---: | ---: |
| CLI fetch of sequence 300,000, indexed | 2.76 ms | 2.69 ms |
| CLI fetch of sequence 300,000, no index | 19.15 ms | 18.95 ms |
| HTTP time to first record at sequence 300,000 | 0.21 ms | 0.19 ms |
| HTTP Lite3 transfer | 55.91 ms | 10.11 ms |
| HTTP JSON transfer | 27.81 ms | 20.58 ms |

The CLI and point HTTP runs alternated the two builds seven times. The
unindexed CLI check used 21 runs per build after warmup. The HTTP transfers
used ten runs per build after warmup. To reproduce the point checks, create a
256 MiB pool with `pool create --size 256M --index-capacity 600000` and seed it
with 300,000 small records. Time `fetch POOL 300000` or the first record of an
HTTP fetch starting at `since=300000`. Set the index capacity to zero to
measure the scan. For HTTP throughput, compare the complete finite `/v2`
fetch with the 1.x finite follow of the same retained records.

Against the original 2.0 candidate (`e4b2429`), a time seek past all 300,000
retained records reduced a concurrent writer's median lock wait from 17.4 ms
to 14.7 ms. The seek still holds a shared lock to select the first matching
record from a consistent opening snapshot. With 64 quiet cancellable HTTP
readers, the client used two native threads and 6.9 MiB of resident memory,
down from 65 threads and 13.2 MiB. All readers still close on cancellation.

## Integration checks

The pre-2.0 owned-view checkpoint passed `just check` and
`just release-gate`, including language bindings, C interface checks, packaged
native and addon-free Node installs, and native/fallback Python wheels. An
additional Node run with explicit garbage collection checks retained children.
The final accepted implementation passes all 127 Node cases on both Node 24
and Node 26, including actual portable-to-native forwarding.
[Recorded commands, wheel hashes and logs](language-boundary-results/owned-views/final-integration.json)
identify this checkpoint; later measured changes need their affected checks.
