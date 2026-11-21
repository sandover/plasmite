# Python views and input performance

The native extension supplies Mapping and Sequence access over immutable Lite3.
Selected fields and unchanged forwarding avoid a complete Python object tree.
Native containers can win repeated lookups and full-object work. The ctypes
fallback preserves the API with extra boundary and buffer costs.

## Native extension results

Median times in microseconds:

| Requested result | Small | Wide | 64 KiB string | 1,000-element array |
| --- | ---: | ---: | ---: | ---: |
| Fresh view, selected field | 2.14 | 31.85 | 7.24 | 14.18 |
| Existing get, selected field | 4.32 | 67.44 | 58.96 | 43.70 |
| Fresh view, complete native data | 2.82 | 77.03 | 10.55 | 35.08 |
| Existing get, complete native data | 4.29 | 67.78 | 59.46 | 44.05 |
| Direct JSON from retained view | 0.50 | 21.22 | 19.35 | 9.92 |
| JSON from retained native data | 1.07 | 25.47 | 30.66 | 17.34 |

Warm two-key view access takes 94–107 ns, versus 28–31 ns for an already decoded
dictionary. Summing 1,000 integers through the native iterator takes 19.56 µs;
a native list takes 1.36 µs. The iterator uses a fixed 16-entry buffer and a
bounded C cursor, while the list exposes contiguous PyObject pointers to `sum`.
The C walker builds explicit native copies without transient child wrappers,
but wide cold conversion still costs 14% more than the JSON API. Keep that
regression visible when choosing an API for full-message consumers.

The string copy comparison has different allocation semantics: Python's
`deepcopy` reuses an existing immutable string, while converting encoded Lite3
creates a Python string. Selected views avoid that decoding until requested.

## Fallback and memory

The ctypes fallback preserves lazy immutable containers and readonly buffers.
It pays for extra encoded copies and per-field ctypes calls. Small cold selected
reads take 8.44 µs versus the existing API's 4.30 µs; wide full-native conversion
takes 1.49 ms versus 68.76 µs. It provides runtime compatibility, not native
extension performance. Its direct JSON route still avoids a Python data tree.

Separate [native](native.allocations.json) and
[fallback](fallback.allocations.json) diagnostics report Python tracemalloc
peaks with **null timings**. They exclude C/Rust allocation and mapped storage;
they do not establish total bytes or allocations per operation. Retained-child
and readonly-buffer checks survive pool closure. Encoded byte lengths identify
the snapshot each child retains; peak process RSS does not measure live retained
owner memory. A small child keeps the entire encoded message alive.

## One `Message` in the public API

The [message benchmark](../../../../../bindings/python/benchmarks/message_surface.py) compares the earlier view-based interface at `94dd55a` with the single-message implementation. Each run used standard CPython 3.14.10 and a freshly built native extension on an Apple M5 Pro Mac. Both libraries used Rust's debug profile. Each cell reports median CPU microseconds per message across five runs. The small input has one integer. The wide input adds 160 objects beside that same integer. Each finite read starts with the pool already populated.

Ordinary iteration now returns one native message directly, without allocating the temporary result list that explicit batches use. A pool also keeps its native address and backend so a feed can reuse both. Ten alternating runs used the same Mac and Python version with release Rust libraries. The `relay` lane read from a populated pool and forwarded every message to a second pool. Small messages took 4.73 → 3.40 µs of CPU per message, and wide messages took 19.75 → 16.66 µs. For a direct feed that reads only the returned sequence, the medians were 2.96 → 2.35 µs for small messages and 24.37 → 23.12 µs for wide messages. Use `--operation relay --repeats 1` for the relay and `--operation feed_metadata --repeats 1` for the direct feed. Alternate builds to repeat the comparison.

| Work | Small before | Small with `Message` | Wide before | Wide with `Message` |
| --- | ---: | ---: | ---: | ---: |
| Feed and read the sequence | 5.62 | 5.83 | 109.82 | 109.34 |
| Forward the stored message and read the new sequence | 3.80 | 4.05 | 6.17 | 6.59 |
| Read only the sequence | 3.47 | 3.46 | 40.45 | 40.43 |
| Read a native Python dict | 4.24 | 4.14 | 153.17 | 155.13 |
| Read only the selected integer | 3.86 | 3.91 | 40.53 | 40.51 |

`Message.field("target")` kept the cost of reading one field from a wide message. The new range was 40.02–41.07 µs, and the prior range was 40.49–51.35 µs. Forwarding added 0.25–0.42 µs to the median. The forwarding ranges were 3.73–3.98 µs before and 4.01–4.21 µs after for the small input, and 6.07–6.31 µs before and 6.51–6.91 µs after for the wide input.

Run the same script against each checkout's built library and native Python extension. The script uses `feed` and `fetch` on the single-message API and the earlier owned-view calls on the baseline. The measured library SHA-256 values were `d2563c00bef0f817b943a8b39cacb5b821160217956f29b5ad5cd9465cf792f5` before and `ce9975ddccbbf0e42c1efab5831f13cc7e0ec212245779ded80672e41a89d5fc` for the working tree.

```sh
PLASMITE_LIB_DIR=/path/to/target/debug PYTHONPATH=/path/to/bindings/python \
  uv run --no-project python bindings/python/benchmarks/message_surface.py
```

## Owned UDP receiving: final central checkpoint

Owned DATA now crosses C/Python as one checked message owner. Native Python
adopts that owner directly; it builds neither a separate Python payload buffer
nor a JSON/native tree before the caller asks for one. The fallback keeps its
documented readonly-buffer copy. Skip/progress events retain their old meaning.

Three fresh release processes per lane alternate lane order and verify all
1,000 sequences and the complete final value. The peer sends one DATA at a time
through a test queue; these figures include loopback delivery and pacing, and
do not estimate UDP saturation throughput. The full-native view and legacy
lanes return the same mutable Python data value.

| Fixture | Raw bytes only | Legacy native data | View: one field | View: native data |
| --- | ---: | ---: | ---: | ---: |
| Small | 21.42 µs | 29.94 µs | 22.70 µs | 24.15 µs |
| Wide | 27.62 µs | 71.26 µs | 28.84 µs | 40.25 µs |
| 32 KiB text | 30.32 µs | 73.79 µs | 28.95 µs | 31.30 µs |
| 1,000 integers | 33.29 µs | 97.17 µs | 34.27 µs | 62.20 µs |

With the same requested native result, wide/text/array receiving takes
44%/58%/36% less time. Reader-thread CPU falls from 57.79/59.00/81.36 µs to
25.07/15.39/44.50 µs per message. Process CPU also includes the peer thread;
[all raw repeats](udp.raw.json) keep both measures and the exact library,
extension, source and worker fingerprints. Small full-native receiving takes
19% less time. Selected reads avoid the requested native copy and therefore
measure a different caller job from full-native conversion.

Build the shared library in release mode and build the native extension for the
selected interpreter. The recorded UDP runs used CPython 3.13. Run one lane
per process:

```sh
PLASMITE_LIB_DIR=/fixed/sdk/lib PYTHONPATH=bindings/python \
  python3.13 bindings/python/benchmarks/udp_views.py view-native wide --count 1000
```

Lanes are `raw`, `legacy`, `view-selected`, and `view-native`; fixtures are
`small`, `wide`, `text32k`, and `array`. Alternate lane order across repeats.
Tests cover exact identity, duplicate/late arrival order, control events and
retained owners after close in native and fallback Python. The timed native
backend does not establish fallback performance.

## Reproduce

Use [bindings/python/benchmarks](../../../../../bindings/python/benchmarks/)
for view access, native input, complete pipelines and streams. The
[native raw runs](native.raw.json), [fallback runs](fallback.raw.json),
[producer summary](producer-summary.json), and [UDP runs](udp.raw.json)
record interpreter, backend and artifact identity. Initial view comparisons
used CPython 3.15.2 on macOS/Apple M5 Pro; they precede later core constructor
changes. Rebuild both candidates and compare matching requested results before
applying those historical numbers to a new change.

[Explicit batch results](batches/README.md) cover complete consumer work,
including the cases where batching adds idle cost or retains more owners.
