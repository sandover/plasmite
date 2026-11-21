# Language interfaces and performance

The `Message` returned by `feed`, `follow` and `fetch` holds an immutable Lite3 snapshot. Reading metadata or forwarding the message can reuse that snapshot. An application that needs ordinary language objects pays for decoding the data once, on demand. The experiments below compare the paths that led to this design. Their lane names describe the operation under measurement.

## Where the work went

A local read copies a record into an independent immutable snapshot and validates
it once. Children retain that snapshot. Checked writers establish the same
invariants while encoding, and forwarding reuses the bytes and proof when the destination keeps the same data and tags and needs no sender change. Pool storage and transports still copy bytes;
“zero-copy JSON” does not describe the complete path.

Each runtime needs its own efficient boundary. Python's native extension uses
container and buffer slots for Mapping/Sequence access. Go keeps immutable bytes
in its own heap and uses coarse native calls for selection and typed decoding.
Its homogeneous scalar arrays cross as compact spans, rather than one general
value descriptor per element. Mutable input snapshots preserve values at the
point the encoder visits them. These changes remove intermediate work without
changing the stored format or rerunning user hooks.

Python's ordinary reader returns one message per call. The native extension
passes that message directly to Python. The historical batch experiment below
measured how much work readers could save by taking several records in one call.

Node keeps waiting off the runtime's shared filesystem/DNS workers. Local streams
check for ready records on four private workers and park empty reads per handle;
UDP waits directly on its socket. This keeps an idle reader from delaying a busy
one. In the 128-reader alternative comparison, the rare active local reader's
median wait falls from about 54 ms to 0.14 ms. The
[integrated scheduling results](language-boundary-results/node-views/README.md#integrated-scheduling-checkpoint)
also record the thread/memory cost and slower tiny-message view cases.

## Earlier matched results

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

## Choose how much of a message to read

Read the sequence and tags directly for bookkeeping. Use `field()` in Rust,
Python, or JavaScript, or decode into a struct with the fields you need in Go. Ask the
message for its full data when your application needs the whole object. To
forward the same data and tags, pass the message itself to `feed`.

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

Lite3 can occupy more bytes than compact JSON. HTTP readers report terminal
errors, and UDP readers deliver messages in arrival order, including duplicates
and late packets. The library guides cover number types, copying, and cleanup.
[Rust](../../README-crate.md#keep-decode-or-forward-a-message) · [Python](../../bindings/python/README.md#message-data-and-forwarding) · [JavaScript](../../bindings/node/README.md#messages) · [Go](../../bindings/go/README.md#message-bytes-and-forwarding)

## Earlier 2.0 candidate measurements

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
the 1.x append calls with the candidate's typed or JSON producer. Typed feed measured 3.47 → 3.68 µs
and JSON-to-view feed 3.78 → 3.97 µs. The ranges overlapped. An independent
cold field read measured 282 → 286 ns. The 2.0 feed samples the clock as
part of each write.

Build with `cargo build --release --example bench_view_streams --example bench_owned_views`.
Run `bench_view_streams local LANE small 10000` for `raw`, `json`,
`view_selected` and `view_full`, and `bench_owned_views 2000` for the
typed-input and sparse-view lanes. These checks cover local Rust paths.

### 2.0 review check

Seven alternating release runs compared the review changes with `ea7ab55` on the same Apple Silicon Mac. `bench_view_streams` read or forwarded 30,000 small messages per run. `bench_owned_views` ran 2,000 operations per lane. The small-message lanes stayed within their prior timing ranges. Opening a cold view over a 64 KiB string improved after Plasmite learned to validate 32 consecutive ASCII bytes at a time.

| Local path | Before, ns per message | After, ns per message |
| --- | ---: | ---: |
| Read encoded Lite3 | 853 | 865 |
| Read decoded message | 1,155 | 1,216 |
| Read one view field | 976 | 973 |
| Decode the full view | 1,209 | 1,227 |
| Forward a view | 5,433 | 5,239 |
| Encode and feed typed input | 4,880 | 4,243 |
| Encode and feed via JSON | 5,144 | 4,318 |
| Open a cold view and read one field, 64 KiB string | 5,911 | 2,119 |

## Single-message API

Twelve paired release runs compared the public Rust `Message` with the earlier interface at `94dd55a`. Each process read or forwarded 30,000 small retained messages on the same Apple Silicon Mac. These are nanoseconds per message. Ranges show the fastest and slowest run.

| Requested work | Earlier API | `Message` |
| --- | ---: | ---: |
| Read the encoded bytes | 606 (591–634) | 748 (738–934) |
| Decode the full JSON object | 790 (771–809) | 925 (915–939) |
| Read one field | 773 (755–795) | 770 (759–814) |
| Decode to a Rust struct | 964 (958–989) | 961 (947–975) |
| Read and forward the message | 2,378 (2,349–2,400) | 2,381 (2,363–2,426) |

The encoded-only and full JSON lanes increased by about 0.14 µs each. The `Message` path carries an owned snapshot and validates its lookup index for later field access. Full JSON conversion reuses that validation and caches the result. Selected-field reads, typed decoding and forwarding overlapped the earlier ranges.

Build `bench_view_streams` in release mode at each revision. Run `bench_view_streams local LANE small 30000`, alternating revisions. Earlier lanes are `raw`, `json`, `view_selected`, `view_full` and `view_forward`. Match them to `message_raw`, `json`, `message_selected`, `message_full` and `message_forward` in the new build. Each run checks the result.

The Python debug comparison found near parity for metadata, native conversion and selected fields. Forwarding added 0.25–0.42 µs to the median. The Go debug comparison found overlapping ranges for selected fields and forwarding. [Python results](language-boundary-results/owned-views/python/README.md#one-message-in-the-public-api), [Go results](language-boundary-results/owned-views/go/README.md).

### Fixed field lookups and single-message reads

The reader now looks up fixed Lite3 envelope keys without copying them and checks tag types without opening an iterator. The Lite3 verifier also uses a short ASCII scan and checks each fixed node once. Seven alternating release runs on the same Mac compared the implementation in `b92a417` with `94dd55a`. Each process read 30,000 small records or 6,000 wide records. The wide fixture adds an object with 200 fields. Figures are median nanoseconds per message.

| Requested work | Small before → after | Wide before → after |
| --- | ---: | ---: |
| Read the encoded bytes | 610 → 693 | 3,207 → 3,566 |
| Decode the full JSON object | 808 → 870 | 13,983 → 14,856 |
| Read one field | 781 → 725 | 3,966 → 3,600 |
| Decode to a Rust struct | 971 → 914 | 17,361 → 16,880 |
| Read and forward the message | 2,415 → 2,331 | 6,840 → 6,451 |

The encoded and full-JSON runs still take longer than the earlier API. The new reader checks the Lite3 lookup index on read so a caller can later use the same message for selected fields, typed decoding, or forwarding. Run the commands above with fixture `small 30000` or `wide 6000` to repeat the comparison.

Nine alternating pairs on the same local Node pool used the same native debug library. At 128 B, feed plus a selected field took 23.3 → 22.7 µs, and fetch plus a selected field took 21.2 → 20.6 µs. At 64 KiB the corresponding comparisons were 99.2 → 97.9 µs and 29.5 → 28.9 µs. The new path used `message.field()` to match the earlier direct view lookup.

#### Catch-up and relay with release builds

Twelve paired runs on an Apple M5 Pro compared `94dd55a` with the single-message implementation. Both used a release native library. Rust drained a prefilled local pool and verified the result. Python 3.14.10 used freshly built native extensions on both revisions and copied a prefilled pool into a second local pool. The timed interval included opening the reader, reading each message and, for Python, appending it to the destination with default `fast` durability. The Python benchmark checked every source and destination sequence and read the first and last copies back outside the timed interval. Fixture creation stayed outside the timed interval.

Wall time in microseconds per message, shown as median and range. The final column counts the pairs where the new API took longer.

| Workload | Messages | Earlier API | `Message` | Slower pairs |
| --- | ---: | ---: | ---: | ---: |
| Rust, read raw bytes, 554 B | 30,000 | 0.601 (0.594–0.680) | 0.755 (0.740–0.852) | 12/12 |
| Rust, read raw bytes, 10,423 B | 5,000 | 3.227 (3.130–4.932) | 3.660 (3.559–4.010) | 11/12 |
| Rust, decode full JSON, 554 B | 30,000 | 0.804 (0.784–0.872) | 0.927 (0.915–0.987) | 12/12 |
| Rust, decode full JSON, 10,423 B | 5,000 | 13.899 (13.592–15.330) | 14.938 (14.679–16.974) | 12/12 |
| Python, relay one-field objects | 5,000 | 4.770 (4.680–6.000) | 5.095 (5.030–5.710) | 11/12 |
| Python, relay objects with 160 nested fields | 800 | 19.280 (18.760–22.320) | 19.545 (18.860–22.810) | 9/12 |

Rust's selected-field and typed-decode controls also ran at both sizes. Their ranges overlapped, with medians within 6% of the earlier API. For Python, CPU time followed wall time. The median paired wall-time increase was 6.8% for small messages and 1.0% for wide messages.

For Rust, run the same `bench_view_streams` lane pairs above with `small 30000` and `wide 5000`. For Python, run [message_surface.py](../../bindings/python/benchmarks/message_surface.py) with `--operation relay --repeats 1`, choosing each revision's native Python package through `PYTHONPATH` and its versioned release library through `PLASMITE_LIB_DIR`. Alternate which revision runs first in each pair.

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
measure the scan. For HTTP throughput, compare a complete 2.0 fetch with a
1.x finite follow of the same retained records.

Against the original 2.0 candidate (`e4b2429`), a time seek past all 300,000
retained records reduced a concurrent writer's median lock wait from 17.4 ms
to 14.7 ms. The seek still holds a shared lock to select the first matching
record from a consistent opening snapshot. With 64 quiet cancellable HTTP
readers, the client used two native threads and 6.9 MiB of resident memory,
down from 65 threads and 13.2 MiB. All readers still close on cancellation.

### Direct decoding and view validation

Local Rust message reads build the owned result from checked Lite3 values.
They keep the previous exporter as a fallback when the direct path cannot
read a legacy document. View validation remembers the checked node at each
tree depth during the walk and still checks every key and value.

Seven interleaved release-build runs against `4c0938b` gave these medians
using `bench_view_streams local`. Both builds ran on the same Apple Silicon
Mac. The x64 binaries ran through Rosetta. Times are µs per message and include
reading the record from the pool.

| Workload | ARM64 before → after | x64 before → after |
| --- | ---: | ---: |
| Decode a message with 32 KB of text | 10.00 → 3.53 | 11.89 → 4.65 |
| Decode a wide object | 18.17 → 14.65 | 40.97 → 35.45 |
| Decode a 1,000-element integer array | 24.06 → 14.06 | 34.81 → 22.00 |
| Open a wide view and read one field | 4.71 → 4.04 | 7.59 → 6.41 |

Use the `json` lane with the `long`, `wide`, and `array` fixtures and
`view_selected` with `wide`. The raw array control and small selected views
had overlapping timing ranges. Compatibility checks compare decoded values
and errors with the previous JSON round trip, including the stored 1.3.0
fixture, exact floats, native bytes, and a legacy count mismatch.

### Direct writes, HTTP JSON and view field reads

Rust `Pool::feed` writes native values with the private Lite3 builder. It
matches the legacy encoder's generation counter by discounting buffer growth
retries when it seals the message. Values outside the private writer's
domain still use the legacy encoder. HTTP JSON reads check the payload once,
then format directly from that checked snapshot. Validated views read scalar
fields from their stored bytes.

These medians compare optimized builds against `a2a2e8c` on the same Apple
Silicon Mac. The x64 builds ran through Rosetta.

| Workload | Before → after |
| --- | ---: |
| Feed a message with 32 KB of text, ARM64 | 21.25 → 6.48 µs |
| Feed a 1,000-integer message, ARM64 | 61.52 → 36.01 µs |
| Feed a message with 32 KB of text, x64 | 26.43 → 10.39 µs |
| Feed a 1,000-integer message, x64 | 88.71 → 53.10 µs |
| Fetch 2,400 wide messages as HTTP JSON over loopback | 38.24 → 31.30 ms |
| Fetch 2,000 integer-array messages as HTTP JSON over loopback | 53.68 → 42.18 ms |
| Sum 1,000 integers from an open view, ARM64 | 10.26 → 8.70 µs |

Feed timing includes the append and complete receipt for 3,000 writes per
run. The view result uses the `view_array_cursor` lane of
`bench_owned_views`. The HTTP runs verify the full response, including its
ending, against the baseline. Feed and HTTP results use seven measured runs per
build. The view result uses six. Small writes and HTTP reads with a tag that
matches no records had overlapping timing ranges.

## Integration checks

The pre-2.0 owned-view checkpoint passed `just check` and
`just release-gate`, including language bindings, C interface checks, packaged
native and addon-free Node installs, and native/fallback Python wheels. An
additional Node run with explicit garbage collection checks retained children.
The final accepted implementation passes all 127 Node cases on both Node 24
and Node 26, including actual portable-to-native forwarding.
[Recorded commands, wheel hashes and logs](language-boundary-results/owned-views/final-integration.json)
identify this checkpoint; later measured changes need their affected checks.
