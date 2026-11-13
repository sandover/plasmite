# Numeric append receipts

A producer often needs confirmation that Plasmite committed a record and the
identity it assigned. A full receipt also returns the producer's complete data.
The optional numeric operation returns only the committed sequence and unsigned
Unix nanoseconds. Consumers still read the complete stored message.

The local Rust implementation encodes once and calls the same storage append.
It uses the committed sequence and the exact timestamp supplied to that append.
It skips cloning input data/tags into a Message, formatting a timestamp,
serializing a receipt, allocating/copying receipt bytes across C and decoding
them in the caller's runtime. Binary input still passes normal validation.
There is no subsequent get, which could fail after another writer overwrites the
record. Each call owns its small output and adds no persistent cache.

Rust returns a `(u64, u64)` pair, matching its existing remote binary identity
operation. C fills a caller-owned structure. Python returns `AppendReceipt` with
integer `seq` and `timestamp_ns`. Node returns bigint `seq` and `timestampNs`.
Go returns `api.AppendReceipt` with uint64 fields. Existing methods preserve
complete receipt data and their snapshot behavior. UDP send cannot report a
committed identity and gains no receipt operation.

New C symbols require a matching library. Python detects the symbols and leaves
older operations available with older libraries; a missing identity symbol fails
before commit. Go helpers test an optional backend method rather than expanding
the required Pool interface. Unsupported backends fail before append. Node and
Go packages need their matching native SDK, as the upgrade guide describes.

## Validation

Rust tests run two independent writers against a small ring, check exact
identities through overwrite, and exercise timestamp values at the u64 limit.
C tests compare numeric results with stored frames, keep output untouched on
invalid JSON/binary/durability, and prove rejected input leaves the sequence
unchanged. Every binding checks real stored identity above 2^53, full data after
caller mutation, Fast and Flush durability, and rejection without sequence
advance. Python also checks an absent ABI symbol; Go checks a backend that only
implements the original interface. Existing lifecycle and complete-message
suites remain the compatibility checks.

## Performance method

`scripts/bench_pipe_comparison.py --receipt identity --baseline-receipt full`
compares the two producer requests against the same optimized library. Each run
uses one producer and one consumer. The consumer decodes and checks every full
record in both cases. The full producer checks echoed data and sequence; the
numeric producer checks sequence and a positive exact integer timestamp.
The benchmark also reports the JSON pipe, which has a different retention
contract. The identity request asks for less producer output, so any gain
belongs to that explicit API choice. It does not make existing full receipts
faster. Setup/imports/startup stay outside timing; both processes finish before
timing ends. Raw files record repeat order, CPU and verified counts.

## Optimized measurements

Seven alternating small runs use 100,000 records each. Five alternating large
runs use 5,000 records each with 64 KiB text. The host uses macOS 27.2, Apple M5
Pro and Python 3.15.2. Both variants use the identical library hash recorded in
the raw files; the only workload difference is the requested receipt.

| Producer request | Small messages/s | Small combined CPU s | Large messages/s | Large combined CPU s |
| --- | ---: | ---: | ---: | ---: |
| Full receipt | 48,501 | 3.317200 | 6,330 | 1.230640 |
| Numeric receipt | 74,010 | 2.498665 | 10,545 | 0.911646 |

Numeric receipts increase complete pipeline throughput 52.6% for small records
and 66.6% for large records, while reducing combined CPU 24.7% and 25.9%.
The pipe reaches 301,520 and 16,191 messages/s on these workloads, respectively.
The remaining gap is about 4.1 times for small records and 1.5 times for large
records. The pipe retains no history. These results establish one Python
producer/consumer workload on one host; they do not establish other bindings'
performance or a universal transport comparison.

[Small samples](small.raw.json) and [large samples](large.raw.json) record every
worker's counts, checksum, CPU, repeat order and build fingerprints. [bench.py](bench.py)
preserves the exact measured harness. When running that saved copy, pass
`--bindings /absolute/path/to/plasmite/bindings/python`; its normal default assumes
the script sits under the repository's scripts directory. The large workload
sets `PLASMITE_BENCH_NOTE_BYTES=65536`. Both libraries in the command below point
to the same optimized build that includes numeric receipts.

```sh
python3 scripts/bench_pipe_comparison.py --messages 100000 --repeats 7 \
  --receipt identity --baseline-receipt full \
  --library LIBRARY --baseline-library LIBRARY --output RESULT
```

No retained-memory or operating-system-wide claim follows from these timing and
CPU measurements. The operation adds no persistent owner, background work or
shared mutable state. Cross-binding allocation/memory measurements belong to
the broader language-boundary benchmark ticket.
