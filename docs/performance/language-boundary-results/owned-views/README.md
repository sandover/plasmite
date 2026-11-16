# Owned-view performance

An owned view keeps one immutable Lite3 snapshot and its validation result.
Selected reads and unchanged forwarding avoid constructing a complete native
object tree. A retained child keeps the whole snapshot alive; native exports
pay for the containers the caller requests.

Use the [language-interface guide](../../language-interfaces.md) to choose a
path. [Architecture](../../../record/architecture.md#immutable-message-ownership)
explains ownership; the [API contract](../../../../spec/api/v0/SPEC.md) defines
behavior. These measurements describe particular builds on one Mac, not a
ranking of languages or a guarantee for every workload.

## Encoding and conversion

Rust writes Serde values directly into Lite3 and decodes requested types from
stored values. Checked writers retain their construction proof. C formats JSON
directly from immutable frames. Sequential cursors avoid repeated array lookups;
short field names avoid temporary heap allocations. Existing object APIs keep
their documented semantics.

The raw comparisons preserve the controls and limits:

- [Rust decoding](rust-decode.raw.json) and [encoding](rust-codec.raw.json).
- [Construction proof: core paths](proven-seal.raw.json) and
  [Rust paths](proven-seal-rust.raw.json).
- [Native builders](native-builder.raw.json) and
  [field-name allocation](key-buffers.raw.json).
- [Validation](strict-proof.raw.json), [JSON conversion](json-capability.raw.json),
  and [sequential iteration](cursor.raw.json).
- [Go](go/README.md), [Python](python/README.md), and
  [Node](../node-views/README.md) report their runtime-specific costs.

## Central Rust stream measurements

The [verified central repeats](rust-streams.raw.json) run each fixed release lane
in three fresh processes with alternating lane order. Local snapshots stay
prefilled outside timing; UDP keeps the one-packet pacing described above. Every
run verifies the requested output, identity/count and final forwarded bytes.

| Local fixture | Raw frame | View: selected field | View: full native data | Legacy native message |
| --- | ---: | ---: | ---: | ---: |
| Small | 0.71 µs | 0.85 µs | 1.04 µs | 1.32 µs |
| Wide | 3.25 µs | 4.36 µs | 18.12 µs | 17.91 µs |
| 32 KiB text | 2.85 µs | 2.99 µs | 3.46 µs | 9.95 µs |
| 1,000 integers | 6.85 µs | 8.00 µs | 23.81 µs | 24.04 µs |

Selected access avoids constructing unrequested fields. Requesting all native
data keeps the wide/array cost close to the legacy control, while long-text
decoding benefits from avoiding JSON formatting and parsing. Legacy messages
also construct metadata/time; view lanes construct only the requested data.
Owned local forwarding takes 8.02/7.44/13.48 µs for wide/text/array, versus
9.38/9.12/17.57 µs when raw forwarding validates the incoming mutable buffer.
These controls have different input ownership guarantees.

UDP selected/full-native wide delivery takes 34.51/50.17 µs versus 47.42 µs for
legacy messages. Full wide conversion therefore regresses about 6% in this
small paced run; array full conversion takes 41.58 versus 55.44 µs. Transport
scheduling dominates several UDP controls: wide raw forwarding even comes out
faster than raw receiving. Keep those raw repeats and their limits visible;
they do not establish a general UDP throughput or tail-latency improvement.
The [C/Python UDP checkpoint](python/README.md#owned-udp-receiving-final-central-checkpoint)
separately measures its language-boundary and requested-conversion costs.

## Reproduce

Build outside the timing window. The benchmark script's help lists C/Go worker
build commands. Then use:

```sh
cargo build --release --example bench_owned_views --example bench_owned_view_allocations --example bench_view_streams
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
