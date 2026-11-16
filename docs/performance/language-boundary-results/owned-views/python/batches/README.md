# Explicit Python view batches

Explicit batches reduce repeated reader locking and boundary calls. Ordinary
iteration remains one record at a time; the caller chooses count and byte bounds.

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

## Final central confirmation

The central measurement pass checks the implementation after filtered-poll,
mixed-reader and callback/cancellation fixes.
Five alternating repeats compare ordinary view iteration with explicit count 16
batches and a 256 KiB byte bound. Producer work and requested consumer conversion
match within each pair; each record still undergoes exact-result checks.
Timing ran without competing builds or benchmarks.

| Requested consumer result | Ordinary records/s | Batch records/s | Throughput change | Ordinary CPU ms | Batch CPU ms | CPU change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| message-json | 70,945 | 120,865 | +70.4% | 87.407 | 71.957 | -17.7% |
| native | 102,241 | 134,932 | +32.0% | 67.997 | 60.480 | -11.1% |

[Whole-message JSON/dataclass raw runs](central-confirmation/message-json.json)
and [native-data raw runs](central-confirmation/native.json) record source,
backend and library hashes. They use library
`2a1dee2cbb01805551dae8960eb35b60fe576f53ba723e55237c117d975b6160`.
The reproduction command below selects `message-json`; change
`--reader-view-conversion native` for the second row. Both use 5000 messages,
five repeats, native-view producer receipts, no producer delay and no pipe.
The recorded environment and library hashes define the scope of these results.

## Reproduce

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/bench_pipe_comparison.py \
  --library /path/to/frozen/lib --baseline-library /path/to/frozen/lib \
  --receipt native-view --baseline-receipt native-view --reader-views \
  --reader-view-conversion message-json --reader-batch-count 16 \
  --baseline-reader-batch-count 0 --reader-batch-bytes 262144 \
  --messages 5000 --repeats 5 --no-pipe --output /tmp/stream-batches.json
```

Use the same fixed library and producer on both sides. Change the conversion
to `native` for the second row. The focused
`bindings/python/benchmarks/stream_batches.py` harness covers fairness, retention
and forwarding; set `PYTHONPATH` and `PLASMITE_LIB_DIR` to the matching package
and library.

An idle reader pays batch bookkeeping without amortizing it. Larger batches keep
more independently owned messages alive. [Paced runs](paced-count-16.json),
[owner retention](retention.json), and [forwarding](forwarding.json) preserve
those controls. The byte bound measures encoded payloads, not total process
memory. Do not infer sparse-read gains from a comparison that requests different
consumer conversions.
