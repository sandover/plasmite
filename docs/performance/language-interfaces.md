# Language interfaces and performance

Owned views help when an application reads a few fields, retains encoded data
or forwards a message. They keep Lite3 and its validation result instead of
rebuilding a whole native object tree at each language boundary. An application
that needs every field as ordinary language objects still pays for that copy.
Existing message APIs remain available; views add a choice, not a new default.

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

## Matched results

Each row compares the same requested result within one recorded experiment.
The fixtures, runtimes and paths differ across rows; this table does not rank
languages. Times and throughput include the work named in the second column.
The linked reports preserve fixed artifacts, verification, raw runs and controls.

| Change | Matched work | Before → after | CPU or allocation context |
| --- | --- | ---: | --- |
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
JSON, so fewer conversions do not guarantee less wire traffic or CPU. HTTP
binary tails keep their terminal-error limits, and UDP views keep arrival-order,
duplicate and late-packet behavior. Views change representation, not delivery.
See the [migration guide](../record/upgrading-next-major.md#keep-encoded-data-with-owned-views)
for exact-number rules, copy boundaries, cleanup and transport support.
