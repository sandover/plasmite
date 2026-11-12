# JSON pipe and ordinary Plasmite Python API

**Reusing the notification name raised Plasmite throughput from 28,821 to
44,327 messages per second and reduced combined worker CPU time by 28% in this
workload. The buffered JSON pipe delivered 300,893 messages per second.**

These November 12, 2026 measurements describe one arm64 Mac, Python 3.15.2,
and an optimized Rust 1.99.1 library. They establish a baseline for one local
pipeline. They do not establish network performance, cross-platform results,
or a general ranking of messaging systems.

## Workload and results

One producer sends 20,000 sensor records to one reader in separate processes.
Each record contains an ID, 17 numeric measurements, nested context, labels,
and a text note. Standard Python JSON encoding occupies 925 bytes for the
first record and grows slightly as IDs increase. Both readers compare every
decoded record with its expected value and sum the same measurement. Plasmite's
writer also checks each ordinary API receipt.

Each lane warms once. Seven measured repeats rotate and reverse the order of
the pipe, saved baseline library, and updated library. The table reports medians;
the range covers all seven throughput observations. CPU time sums the producer's
and reader's user and system CPU time during the measured work.

| Path | Messages/s, median (range) | Elapsed time for 20,000 | Combined CPU time |
| --- | ---: | ---: | ---: |
| Buffered JSON pipe | 300,893 (298,042–308,409) | 0.0665 s | 0.1276 s |
| Plasmite before | 28,821 (28,183–29,219) | 0.6939 s | 0.8790 s |
| Plasmite after | 44,327 (43,519–45,284) | 0.4512 s | 0.6304 s |

Every measured reader verified all 20,000 messages: no missing, reordered,
duplicated, or changed records. The updated Plasmite path delivered 53.8% more
messages per second, took 35.0% less elapsed time, and used 28.3% less combined
CPU time. Its writer CPU median fell from 0.6589 to 0.4152 seconds; reader CPU
remained close, at 0.2216 and 0.2176 seconds.

The pipe uses Python's ordinary buffered binary stdout. It batches writes and
retains no history. Plasmite uses normal `Pool.append()` and `Pool.tail()` calls,
including their JSON conversion, message envelopes, timestamps, receipts, and
owned snapshots. It stores records in a file-backed pool with fast durability;
it does not wait for a disk flush after each append. Each trial creates a fresh
156.25 MiB pool, large enough to retain the complete run.

Timing starts after both processes finish imports and open their resources.
It ends when both finish producing or consuming. It includes per-record
serialization, Python/native calls, storage, waiting, decoding, and validation.
It excludes process startup, pool creation, teardown, and profile-file output.
This is a throughput comparison; it measures no per-message latency percentiles
or power-loss durability.

## What the timing found

Separate cProfile runs located most writer time inside the native append call.
Temporary native timing probes then measured append stages over 20,000 records:

| Stage | Median per append |
| --- | ---: |
| JSON-envelope encoding to Lite3 | 2.42 microseconds |
| Acquire writer lock and check the pathname | 2.92 microseconds |
| Commit frame/header and release lock | 0.46 microseconds |
| Notify readers | 12.54 microseconds |

A second 2,000-append diagnostic separated notification work: deriving the
canonical name took 12.75 microseconds, opening the semaphore took 0.79, and
posting/closing took 0.38. Every notification succeeded. These diagnostic runs
had no concurrent reader and used timing probes and stderr output; their
individual medians must not serve as an additive CPU profile or throughput
result. The uninstrumented comparison above establishes the improvement.

The change adds one owned name to each pool. Pool creation/opening derives it
from the existing canonical-path naming rule. Appends reuse it and retain the
existing file-identity checks, locks, commit order, semaphore operations, and
polling fallback. Each reader already chooses its notification name when opening
its tail. The change introduces no public API, file-format, or delivery-contract
change. All timing probes were removed before the comparison builds.

## Reproduce

From the repository root:

```sh
cargo build --release --lib --offline
python3 scripts/bench_pipe_comparison.py --messages 20000 --repeats 7 \
  --output tmp/pipe-comparison.json
```

To compare an earlier library with the current one in the same session, save
its `libplasmite.dylib` or `libplasmite.so` before rebuilding, then add
`--baseline-library /absolute/path/to/saved-library-directory`. Add `--profile`
to collect a separate Plasmite trial with writer and reader cProfile files.
The script uses only Python's standard library and the repository's Python
bindings. It supports Unix hosts and removes its temporary pools and directories.

The [paired raw results](pipe-comparison-results/paired.raw.json) identify the
exact measured library hashes. The baseline came from the existing dirty working
tree at `ddcc67f`, including prior uncommitted work; HEAD alone does not reproduce
it. The optimization patch (outside Git) records
the production-source difference. The results directory also preserves initial
before/after sessions, native timing summaries, source fingerprints, and an
archive of diagnostic patches, raw samples, and profiles. Compiled libraries
are not part of the evidence archive.

## Validation

`just check` passed, including formatting, Clippy with warnings denied, Rust
tests, harness checks, version/distribution checks, and vendored-source integrity.
`just bindings-python-test` ran 49 tests with one skip and passed. The benchmark
itself checked every decoded record and append receipt.

The first sandboxed check could not bind two disposable test listeners. The
first unrestricted check hit a service-log broken-pipe timeout; the focused
retry and subsequent complete check passed. The diagnostics archive retains
these logs. This work changes no service-log code.

## Remaining path costs and reader wake-ups

A follow-up kept the same sensor record and ordinary Python API. It increased
each throughput trial to 100,000 records to reduce the effect of the reader's
final polling wait. Seven rotating/reversed repeats compared a saved library
and Python bindings from the first optimization with the updated implementation.

| Path | Messages/s, median (range) | Combined CPU for 100,000 records |
| --- | ---: | ---: |
| Buffered JSON pipe | 296,838 (279,764–303,885) | 0.6476 s |
| Plasmite with cached notification name | 44,836 (42,251–45,892) | 3.3045 s |
| Plasmite with binding cleanup and notifications | 46,208 (45,472–46,667) | 3.3801 s |

Throughput rose 3.1%; combined CPU rose 2.3%. This is a small throughput gain,
with overlapping ranges. The substantial gain concerns delivery latency.
Every reader verified all records and their ordering; each writer checked its
receipts. The [raw runs](pipe-comparison-results/followup-final.raw.json)
identify both library and binding hashes. These timings use normal release
compiler settings.

### Latency under paced traffic

Five measured repeats sent 100 sensor records at 50 messages per second. Each
record added a monotonic send timestamp. The reader sampled time after the API
returned the record, then verified its full data and order. This includes encoding,
storage, scheduling, native/Python conversion, and delivery. The pipe flushed
each record; the throughput pipe retains its normal buffering. Each path warmed
once. Values below are the median of each repeat's median or 95th percentile.

| Path | Median delivery latency | 95th percentile |
| --- | ---: | ---: |
| Flushed JSON pipe | 0.121 ms | 0.228 ms |
| Plasmite before follow-up | 26.870 ms | 49.945 ms |
| Plasmite with notifications | 4.167 ms | 6.427 ms |

The native C interface had disabled notifications for both JSON and Lite3
streams. Its readers checked for new messages at 50 ms intervals. Enabling the
existing notification path reduced median latency 84.5% and the 95th percentile
87.1%. The [paced runs](pipe-comparison-results/followup-paced.raw.json)
preserve every latency sample. They do not establish a hard deadline.

The Unix notification backend emulates a timed semaphore wait with 5 ms checks.
An experiment reduced that interval to 1 ms: three repeats measured 0.999 ms
median latency and 1.441 ms at the 95th percentile, versus 4.147/6.369 ms with
5 ms checks in the same session. The change increased idle polling substantially,
so the implementation retains 5 ms checks.

Three separate idle trials per variant waited for two seconds without a writer.
All variants used the updated Python wrapper. Median CPU consumption was
0.065% of one core for the original notification-disabled stream, 0.259% with
5 ms notification checks, and 0.837% with 1 ms checks. The retained latency
improvement therefore costs about 0.19 percentage points of one core per idle
reader on this Mac. The [idle samples](pipe-comparison-results/followup-idle.raw.json)
and [1 ms experiment](pipe-comparison-results/followup-wake-paced.raw.json)
record this tradeoff. Lowering the global sleep interval does not solve efficient
event-driven wake-up across platforms.

### Why the bulk throughput gap remains

The pipe serializes one Python object, transfers buffered bytes, and decodes one
Python object. The ordinary Plasmite path performs more representation work:

```text
Writer: Python object -> JSON -> Rust object -> JSON -> Lite3 -> pool
Receipt: Rust object -> envelope JSON -> Python Message
Reader: pool snapshot -> JSON -> Rust object -> envelope JSON -> Python Message
```

The pipe amortizes system calls across several records. Plasmite locks, checks
file identity, commits a frame/header, posts a notification, and constructs a
receipt for each append. Its reader returns an owned snapshot and a complete
message envelope. This workload uses fast durability and does not wait for a
disk flush after each message.

Temporary timing probes over 20,000 appends measured medians of 1.38 microseconds
for native input parsing, 2.54 for JSON-envelope/Lite3 encoding, 4.96 for lock,
commit and notification, 0.58 for receipt cloning/timestamp formatting, and 1.13
for receipt serialization. These separate diagnostic timings exclude Python
work and have instrumentation overhead; they do not form an additive CPU
profile. The [stage summary](pipe-comparison-results/followup-budget.json)
and diagnostic archive preserve the probes. The first buffer estimate for the
sensor record was 1,810 bytes, below its 1,948-byte Lite3 body, requiring an
encoder retry. That retry remains a target for work that avoids excess allocation
across other record shapes.

The retained cleanup removes an input copy at the ctypes boundary, replaces
temporary ctypes output-array wrappers with direct owned byte copies, and
avoids regular-expression timestamp normalization on Python 3.11 and newer.
Python 3.10 keeps its normalization path. The Lite3 JSON encoder borrows its
length-delimited input instead of allocating a terminated copy. These changes
preserve owned messages and native argument validation.

A wrapper-only trial reduced combined CPU 4.9%, with a small throughput gain.
Shared file-descriptor ownership and thin link-time optimization showed no clear
additional benefit, so neither ships. An allocation-rounding run lost its build
artifact to a separate build-directory cleanup before completion; it contributes
no result. Later runs used saved libraries and an isolated build directory.

The largest remaining throughput targets are the repeated JSON/object conversions
at the native boundary and per-record commit overhead. A different encoding path
or explicit batching needs its own measured comparison; the current ordinary
Python API still pays those costs.

### Repeat the follow-up

Use `--baseline-bindings /path/to/saved-python-package-parent` alongside
`--baseline-library` when comparing Python wrapper changes. The parent directory
must contain the `plasmite` package. Add `--interval-ms 20 --messages 100` to measure
paced delivery at 50 records per second. The runner flushes pipe writes in paced
mode and records every latency sample. Bulk mode keeps its original record shape
and buffered pipe behavior.

`just check` and all 50 Python binding tests passed with one Python test skipped.
New checks cover length-delimited JSON slices, embedded zero bytes in Lite3,
mutable input conversion, owned results, and the Python 3.10 timestamp fallback.
The runner also completed a paced 3,000-record smoke trial whose reports exceed
a pipe buffer, exercising its concurrent stderr draining. The follow-up evidence
includes the measured harness version, saved source deltas, and diagnostic logs.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
