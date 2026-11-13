# Explicit Python view batches

An explicit batch raises small-record delivery from 70,454 to 120,163 records/s
on this host and cuts combined producer/consumer CPU 18%. It reduces reader
lock handoffs while preserving the same requested consumer result. An idle
reader pays extra overhead, so ordinary iteration still acquires one record.

## Public behavior

`pool.tail_views(...)` returns an iterator and context manager. `next(stream)`
acquires one independent immutable snapshot. `stream.next_batch(count, bytes)`
waits for the first accepted record, then takes available companions without
waiting to fill. Both bounds must be positive; the byte bound counts encoded
payload lengths. An oversized first requested record raises without consuming
it. Records before `since_seq` undergo frame-header checks and skip the copy.

A finite per-call timeout returns `[]` when its wait expires; zero polls.
Finish and cancellation raise `StopIteration`. Existing tail state owns
sequence/gap handling, accepted-message limits, cancellation and the stream-wide
deadline. A batch commits checkpoints one record at a time. If a later record
fails, it returns the valid prefix and leaves the failing record for the next
call. Formatting and validation run after releasing the shared pool lock.

The C surface adds one tagged opener and one bounded owner read, and reuses
existing stream cancel/free. It returns separately retained owners. Neither
ordinary iteration nor later calls keep a hidden prefetched queue. The native
Python adapter creates wrappers directly from those owners; the compatibility
adapter adds its documented encoded buffer copy.

## Matched delivery measurements

CPython 3.15.2, macOS 27.2, Apple M5 Pro; five alternating repeats after one warmup.
These numbers measure the archived stream checkpoint. Final correctness fixes
make zero-timeout polls scan available filtered records, keep mixed C JSON/view
reads on the same filtered checkpoint, and finish Python callbacks before
borrowing handles. The final central confirmation below retests that checkpoint.
Other agents stopped builds/tests/timing. Every row uses the same frozen
library, producer, data and requested consumer conversion. Small/wide/string
rows format each view as a whole JSON message, encode UTF-8, and run the same
existing JSON/dataclass/datetime/raw-message parser. The native-copy row asks
both readers for complete native data instead. Every record undergoes exact
field and sequence checks. Process startup, imports and pool creation stay
outside timing; sending and consumption remain inside it.

| Fixture and requested result | Ordinary views records/s | Batch records/s | Throughput change | Total CPU change |
| --- | ---: | ---: | ---: | ---: |
| Small, count 1, whole-message conversion | 68,900 | 69,188 | +0.4% | -5.1% |
| Small, count 4, whole-message conversion | 71,537 | 90,090 | +25.9% | -5.5% |
| Small, count 16, whole-message conversion | 70,454 | 120,163 | +70.6% | -18.3% |
| Wide 200 fields, count 16, whole-message conversion | 24,110 | 26,416 | +9.6% | -5.2% |
| 64 KiB string, count 16, whole-message conversion | 11,725 | 12,196 | +4.0% | -0.5% |
| Small, count 16, native data | 99,624 | 110,610 | +11.0% | -3.0% |

The count-one row compares the explicit batch entry point with normal iteration;
its CPU difference varies more than its throughput. It does not justify batching
one record. Byte limits can cap an available batch below its requested count.
The earlier JSON-only prototype compares 4 KiB/16 KiB bounds below; those numbers
use a different result path and remain separate evidence.

At 1,000 records/s, throughput stays unchanged and CPU rises 6.1%. Median latency
changes 50.8→51.7µs, p95 187→133µs and p99 391→530µs. These figures summarize only
three 200-record runs; the noisy tails do not establish a latency improvement.
No artificial producer delay or batch-fill delay ships.

[Raw runs](small-count-16.json), [count 1](small-count-1.json),
[count 4](small-count-4.json), [wide](wide-count-16.json),
[string](large-count-16.json), [native conversion](native-copy-count-16.json),
and [paced delivery](paced-count-16.json) include hashes and all samples.

## Why it gains

A [separate instrumented run](locks-count-16.json) records 5,003→317 reader shared
lock acquisitions for 5,000 records, with inclusive acquisition budgets 23.23→2.64 ms.
Writer exclusive acquisitions and wakes remain 5,000. Its exclusive acquisition
budget moves 3.08→3.47 ms; this sample does not show a writer lock-time gain.
The batch amortizes reader crossings and handoffs; it does not reduce wake calls.
Discard all elapsed/CPU throughput fields in this instrumented file: the probe
changes timing. The ordinary runs above contain no probe.

A [two-reader run](fairness.json) gives each independent process the same 3,000
exact messages. Both readers finish in every alternating repeat. The median
writer time drops 30.48→27.69 ms; the slower reader finishes 35.73→27.61 ms; combined
CPU drops 61.11→53.72 ms. This small contention test checks progress under two
readers; it does not establish fairness across arbitrary writer/reader counts.

The [retention diagnostic](retention.json) returns six 680-byte encoded owners
under a 4,096-byte bound. One child stays readable after discarding the batch and
closing stream, pool and client. Its source encoded payload has 680 bytes; this
number excludes alignment, spare allocation capacity, owner metadata and Python
wrappers. Tracemalloc reports Python allocations only, with null timings.
The count/byte bounds cap one explicit acquisition; callers may retain results
from many calls and therefore retain more memory.

## Forwarding

Whole-message input with omitted tags shares the immutable encoded owner.
Explicit tags or an object-root input copy checked data into a new encoding.
Nested object/array views copy checked subtrees directly, and sliced arrays and
byte buffers copy their logical selection. None of these paths build a native
data tree or route through JSON. Later input mutation cannot change a result.

Median send times in microseconds from [five alternating repeats](forwarding.json):

| Input | Whole-owner forwarding | Checked object-root copy | Native-copy then encode |
| --- | ---: | ---: | ---: |
| Small | 5.19 | 5.58 | 6.01 |
| 200 nested fields | 4.90 | 29.07 | 59.53 |
| 64 KiB string | 4.93 | 12.80 | 16.42 |

Each lane requests a retained committed view with the same data and tags.
Whole forwarding preserves the existing encoding; the other lanes create an
encoding, so the difference follows the caller's requested operation.

## Reproduction and history

[Environment and core fingerprints](environment.json),
exact shared-source archive (outside Git), and
exact measured Python source/extension (outside Git)
identify this checkpoint. The Python archive also carries the measured runners.
The extension uses CPython 3.11 headers and the `cp311-abi3` stable ABI.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/bench_pipe_comparison.py \
  --library /path/to/frozen/lib --baseline-library /path/to/frozen/lib \
  --receipt native-view --baseline-receipt native-view --reader-views \
  --reader-batch-count 16 --baseline-reader-batch-count 0 \
  --messages 5000 --repeats 5 --no-pipe --output /tmp/stream-batches.json
```

Set `PLASMITE_BENCH_EXTRA_FIELDS=200` or `PLASMITE_BENCH_NOTE_BYTES=65536` for the
larger fixtures. `bindings/python/benchmarks/stream_batches.py` supplies
`--mode fairness`, `retention`, and `forwarding`; set `PYTHONPATH` and
`PLASMITE_LIB_DIR` to the matching package/library.

The earlier JSON prototype (outside Git) retains its own exact
source and [environment](json-prototype/environment.json). It kept the existing
JSON consumer and acquired independently formatted JSON buffers. Its count 16
small pipeline gained 61%, with 5003→695 reader locks; 4 KiB/16 KiB byte bounds gained
16%/51%. At 1 kHz it cost 15% more CPU. This experiment established the handoff
mechanism before the public owner-returning path. It added no public JSON batch
API and does not supply the final owner-path performance claim.

## Final correctness and packaging checkpoint

[Validation records](validation.json), final core source (outside Git),
[final build fingerprints](final-environment.json), and
final Python source/extension (outside Git) identify the checked
implementation after those fixes. The C stream/owner signatures stay fixed.

The full Python suite passes 95 tests with one expected skip for a Windows-only test on this
Mac. Native and fallback each pass 12 stream cases, including concurrent writers,
retention gaps, cancellation during waits/acquisition, filtered polling, mixed
read methods, byte-bound retries and independent child lifetime. Free-threaded
CPython 3.14.10t passes 43 view/input/stream/guard cases while keeping the GIL
disabled and the native extension unloaded. The library suite covers 275 tests;
two loopback-bind cases require socket permission and pass on rerun. All-target
Clippy passes.

All three wheel variants rebuild from the source distribution and pass SDK byte,
architecture, deployment-floor and tag checks. Installed wheels exercise binary
input, whole-owner forwarding, logical array/byte slices, bounded streams and
child lifetime. The ABI3 wheel also passes the bundled server restart smoke.
The logs preserve Python (outside Git), free-threaded (outside Git)
and wheel (outside Git) results.

## Final central confirmation

The central measurement pass repeats the final implementation with the frozen
SDK above, after the filtered-poll, mixed-reader and callback/cancellation fixes.
Five alternating repeats compare ordinary view iteration with explicit count 16
batches and a 256 KiB byte bound. Producer work and requested consumer conversion
match within each pair; each record still undergoes exact-result checks.
Other agents stopped CPU-heavy work during this pass.

| Requested consumer result | Ordinary records/s | Batch records/s | Throughput change | Ordinary CPU ms | Batch CPU ms | CPU change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| message-json | 70,945 | 120,865 | +70.4% | 87.407 | 71.957 | -17.7% |
| native | 102,241 | 134,932 | +32.0% | 67.997 | 60.480 | -11.1% |

[Whole-message JSON/dataclass raw runs](central-confirmation/message-json.json)
and [native-data raw runs](central-confirmation/native.json) record source,
backend and library hashes. They use library
`2a1dee2cbb01805551dae8960eb35b60fe576f53ba723e55237c117d975b6160`.
The reproduction command above selects `message-json`; change
`--reader-view-conversion native` for the second row. Both use 5000 messages,
five repeats, native-view producer receipts, no producer delay and no pipe.
The final native-data gain differs from the earlier checkpoint's 11% result;
use these final matched numbers when reporting the shipped implementation.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
