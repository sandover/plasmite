# Local follower wake-ups

Local followers can sleep until the operating system wakes them. These
measurements show the delivered-message latency, idle work, and writer
throughput of the notification paths on each tested host.

## Live stream with one and four readers

The local benchmark runs one publisher and starts each reader in its own
process. Readers follow new messages through the C API and start waiting
before the first publish. The publisher sends 1,500 messages at 250 messages/s
in each run. Each message carries a 1 KiB payload plus a sequence and a timestamp.
Each reader checks the pool sequence, the application sequence, and the payload.

Latency starts just before the publisher encodes the input and ends when the
last reader receives the complete JSON message from the C API. The benchmark
uses the same monotonic clock across processes. It measures idle process CPU
separately over eight seconds on the empty pool before each run, after startup.

Five runs per reader count on an Apple M5 Pro Mac (macOS 27.2.1, release build
from `11b9c06d769a23d8b5bd723c9fa11b3501c693b2`) gave these results:

| Independent readers | Median p95 to reach all readers | Deliveries across five runs |
| ---: | ---: | ---: |
| 1 | 0.128 ms | 7,500 / 7,500 |
| 4 | 0.192 ms | 30,000 / 30,000 |

Each p95 run result came from 1,500 messages. The one-reader p95 ranged from
0.116 to 0.155 ms, and the four-reader p95 ranged from 0.166 to 0.213 ms.
In all ten runs, every reader received the final message within 0.34 ms.
One four-reader sample took 4.62 ms to reach all readers, longer than the
4 ms publish interval. Idle CPU ranged from 0.054% to 0.098% of one core per
reader across 25 samples, with a median of 0.078%.

Build the release library and pass its path to the existing harness:

```sh
cargo build --release --lib
python3 docs/performance/language-boundary-results/wakeups/native-follow.py \
  --live target/release/libplasmite.dylib --output /tmp/plasmite-live.json
```

The output records per-message latency for every reader and identifies the
release library by SHA-256. The measured library hash was
`ccd7701ec4568a5f201dece23286231e53a6424f51331fb51737f41951f3f60d`.

## Matched native follower benchmark

Run [native-follow.py](native-follow.py) with the paths to two release
libraries on the same machine. Use Python of the same architecture as the
libraries. The harness verifies every sequence. It records the last of four
independent readers on each paced message, idle process CPU, and the peak
resident memory of each worker. The saturated runs also record writer
completion and last-reader completion. The pools retain all test messages.

```sh
python3 native-follow.py /path/to/baseline /path/to/candidate --mixed --output runs.json
```

On Unix the idle switch count comes from `getrusage` and includes the
benchmark control thread sampling each endpoint. Windows reports process CPU
and peak working set. Process memory includes mapped pool pages. Physical
energy use requires separate hardware measurement.

### macOS conditional wake-all

On the Apple Silicon Mac, five alternating release-build runs compared
unconditional wake-all with the shared wait bit. Each saturated run sent
20,000 messages. Throughput below uses the time until the last reader caught
up when four readers were present.

| macOS workload | Before | With the wait bit |
| --- | ---: | ---: |
| Ordinary writes, zero readers | 257,300 msg/s | 300,100 msg/s |
| Pre-encoded writes, zero readers | 266,300 msg/s | 308,100 msg/s |
| Ordinary writes and four readers | 61,700 msg/s | 66,300 msg/s |
| Pre-encoded writes and four readers | 73,300 msg/s | 75,400 msg/s |
| Paced delivery to all four readers, p95 | 0.35 ms | 0.44 ms |

Median peak memory per worker changed by at most 0.33 MiB in each lane.
Cancellation through the C API still checks a flag, and idle CPU stayed near
0.1% of a core per paced reader. Each mixed-version run delivered all 96
expected messages. With an older writer and newer readers, four-reader p95
was 0.67 ms. With a newer writer and older readers, it was 52.0 ms.

The CLI uses a longer recovery check when standard output is a regular file
and the reader has no cancellation flag. Over one eight-second quiet sample,
macOS `proc_pid_rusage` reported 152 interrupt wake-ups before and 8 after.
Both readers then received the first message and exited. The x86-64 release
test also recovered a commit after writer exit under Rosetta.

### Windows generation events

Five alternating runs compared timer polling with generation events using
x64 release libraries on an ARM64 Windows 11 VM. The benchmark used the
high-resolution performance counter for cross-process timing. Absolute
throughput describes the emulated x64 process on this host.

| Windows workload | Before | With generation events |
| --- | ---: | ---: |
| Paced delivery to one reader, p95 | 48.31 ms | 1.11 ms |
| Paced delivery to all four readers, p95 | 49.96 ms | 2.53 ms |
| Slowest observed paced cancellation | 63.89 ms | 0.69 ms |
| Ordinary writes, zero readers | 29,000 msg/s | 30,200 msg/s |
| Pre-encoded writes, zero readers | 29,700 msg/s | 29,900 msg/s |
| Ordinary writes and four readers, complete drain | 10,170 msg/s | 9,077 msg/s |
| Pre-encoded writes and four readers, complete drain | 10,397 msg/s | 8,799 msg/s |

Peak working set changed by about 0.2 MiB or less per worker. Process CPU
accounting on this VM was too coarse over the quiet samples to quantify an
idle CPU savings. The named events trade 11–15% of drain throughput in the
saturated four-reader case for much quicker notification and cancellation.
Caching the held file's identity avoids repeating filesystem queries on
the writer's wake-up path.

Both mixed-version runs delivered all 96 expected messages. An older writer
and newer readers fell back to the one-second recovery check, with a 937 ms
p95. Newer writers and older readers saw a 52 ms p95 on the older reader's
check. The crash test verified that a reader discovers a committed message
once even when the writer exits before signalling.

## Wake-all on Linux

An idle reader arms a shared bit. The next writer clears the bit when it
publishes the new sequence and wakes every waiting reader when the bit was
set. Writers skip the wake call when the bit is clear. Readers still check
the pool after one second so a writer that exits after committing cannot
leave a reader asleep indefinitely. The C reader can wake immediately on
cancellation on kernels with `futex_waitv`.

The desktop backend work also reran the native benchmark on the Debian ARM64
VM. Both builds used the Linux wake-all protocol. Across three repeats the
median peak resident memory changed by 0.12 MiB or less in every worker lane.
Four-reader p95 ran from 6.59 to 7.35 ms on this loaded VM. The ordinary
four-reader drain moved from 69,600 to 67,400 messages/s, while the pre-encoded
drain moved from 76,300 to 90,700 messages/s. Treat this as a regression
check for the shared-code refactor. The large spread across workloads and
concurrent host activity make it unsuitable for a Linux speedup claim. The full Debian ARM64 Rust
suite passed. Linux x64 and ARMv7 both passed the forced cancellation
fallback and writer-exit recovery tests under emulation.

## Earlier blocking semaphore experiment on Linux

The first Linux experiment put idle followers to sleep in the kernel. Each
append could wake one waiting follower. Followers checked every 50 ms for
cancellation or a message whose wake-up another follower consumed.

The matched run used a Debian 13 ARM64 VM. Both release builds used the
same 2.0 source based on `e358359`. Only the semaphore wait differed.
Each of three paired runs per reader count measured two idle seconds and
then sent eight events about a second apart. Each follower opened its own
pool handle and verified every message. Idle counts come from voluntary
thread sleeps recorded by Linux. The VM measured CPU time and delivery
latency.

| Followers | Idle sleeps/s per follower | Idle CPU per follower | Delivery p50 | Delivery p95 |
| --- | ---: | ---: | ---: | ---: |
| 1 | 185.5 → 19.5 | 0.343% → 0.139% | 2.74 → 0.17 ms | 5.31 → 0.26 ms |
| 4 | 186.5 → 20.0 | 0.273% → 0.101% | 16.30 → 23.75 ms | 51.15 → 47.75 ms |

The table compares 5 ms polling with the blocking wait. Idle figures are
medians across followers and runs. CPU is a share of one guest CPU. Delivery
percentiles combine samples for each follower count and build. Every follower
received every event and stopped within 50 ms of cancellation in these runs.
The four-follower median ran slower in this repeat. Which follower receives
the wake-up still depends on scheduling. These measurements cover VM behavior
and give no estimate of power draw on a physical device.

<a id="adopted-macos-path"></a>

## Earlier unconditional macOS wake-all

On macOS 14.4 and later, the writer publishes the existing aligned eight-byte
newest-sequence word with a release store, unlocks the pool, and calls the public
shared-address wake-all operation. The tail passes the generation from its
empty-cursor check to the compare-and-wait operation. An append before the wait
changes that value; an append after registration wakes the reader. Each reader
then checks its own cursor under the existing storage lock. Wake-ups never
acknowledge delivery or replace the owned frame snapshot.

Each notification owns a 4 KiB header mapping, which can outlive its originating
Pool without retaining another descriptor. The writer uses its existing mapping.
The change adds no thread, shared waiter count, coordination file, or format
version. Waits retain their existing deadlines and at most 50 ms cancellation
slices. Unavailable or failed hints retain bounded polling. Other Unix hosts without
shared-address waits use 5 ms semaphore polling. Windows used a timer at the
time of the macOS measurements below.

The bridge compiles against the supported minimum macOS version, rather than
the installed SDK's default. An initial development build inherited the SDK's
27.2 minimum for this C object, which removed its runtime guards despite Rust's
11.0 minimum. The final build explicitly sets the bridge's deployment target.
[compatibility.raw.json](compatibility.raw.json) verifies both weak imports,
the availability check and the final library's 11.0 minimum. The macOS
measurements below only cover the tested host. Weak imports and
guarded calls establish the build mechanism, not an older-host execution result.

## Matched latency and idle CPU

The final measurements compare the preceding borrowed-descriptor library with
the guarded native-wait build on the same macOS host. The Python pipeline uses
separate producer and consumer processes, the same generated records, numeric
append receipts and full consumer verification. Five alternating repeats send
150 records at 20 ms intervals. Each path warms once before measurement. The
latency starts immediately before producer encoding and ends after consumer
object construction, before full record verification.

| Path | p50 ms | p95 ms | p99 ms | Combined CPU s |
| --- | ---: | ---: | ---: | ---: |
| Record-flushed pipe | 0.161 | 0.241 | 0.338 | 0.029741 |
| Previous semaphore | 4.061 | 6.481 | 6.641 | 0.075096 |
| Shared-address wait | 0.389 | 0.587 | 2.645 | 0.066500 |

The p95 falls 91%; combined CPU falls 11%. The remaining p99 includes scheduling
and the complete conversion path; the native wait does not eliminate that work.
[paced.raw.json](paced.raw.json) preserves individual delivery samples and runs.

[fanout.py](fanout.py) uses three independently opened reader processes. Each
reader checks every sequence and full data. Three alternating repeats each send
150 records at 20 ms intervals, followed by an empty-pool one-second wait per
reader to measure idle CPU. The table reports medians across the nine per-reader
reports, rather than a percentile over a merged sample set.

| Three-reader path | p50 ms | p95 ms | p99 ms | Idle CPU per reader, s/s |
| --- | ---: | ---: | ---: | ---: |
| Previous semaphore | 17.556 | 46.903 | 52.688 | 0.003744 |
| Shared-address wait | 0.307 | 0.541 | 2.604 | 0.001888 |

The semaphore consumes one permit for one waiter. The shared-address operation
wakes every registered waiter, so independent readers no longer depend on their
50 ms cursor recheck when another reader consumes a permit. Idle CPU falls 50%.
[fanout.raw.json](fanout.raw.json) retains all worker reports and exact library
hashes. Mixed old-writer/new-reader and new-writer/old-reader runs both deliver
all 150 records to all three readers. Their different hints do not interoperate:
the fallback produces per-reader p95 around 50 ms. Keep that compatibility
limit separate from same-version latency.

## Writer cost and loaded throughput

Each saturated run sends and verifies 100,000 records. Binary and numeric runs
use seven alternating repeats; complete JSON receipts use five. Imports, process
startup and pool creation stay outside timing. No builds or other experiments
run alongside these measured workers. Background host load still contributes
variation; the numeric baseline spans 61,162–76,020 messages/s, so its median
alone cannot establish a small throughput difference.

| Requested work | Previous messages/s | Native messages/s | Previous/native combined CPU s |
| --- | ---: | ---: | ---: |
| Complete JSON receipt and consumer | 47,503 | 46,566 | 3.444070 / 3.622588 |
| Numeric receipt and complete consumer | 70,896 | 70,621 | 2.602397 / 2.597842 |
| Binary forwarding and raw consumer | 139,580 | 128,316 | 1.034414 / 1.083014 |

The binary path loses 8.1% throughput and uses 4.7% more combined CPU. Its writer
uses 0.641 CPU seconds instead of 0.596; reader CPU stays near 0.442 seconds.
Full JSON loses 2.0% throughput and uses 5.2% more CPU. The numeric medians remain
within host variation. These results support adoption for much lower delivery
latency and idle CPU, with an explicit bulk-writer cost. They do not support a
claim that every workload gets faster. [binary.raw.json](binary.raw.json),
[full.raw.json](full.raw.json) and [identity.raw.json](identity.raw.json) contain
all repeats, worker CPU and peak resident memory.

The separate injected [native-wait-probe.c](native-wait-probe.c) counts operations;
its timing never serves as a performance result. In the 10,000-record saturated
diagnostic, the old writer posts its semaphore 10,000 times; the new writer calls
wake-all 10,000 times. Readers call sem_trywait once or native wait five times.
For 30 paced records, the old reader calls sem_trywait 127 times; the new reader
calls native wait 29 times. These counts explain the different costs: accumulated
semaphore permits can remain in userspace under load, while wake-all enters the
kernel even without a sleeping reader. Native waits avoid periodic 5 ms retries
when delivery is sparse. See [operations.raw.json](operations.raw.json) and
[paced-operations.raw.json](paced-operations.raw.json). The probe counts the
listed API entries; it does not trace every kernel operation.

## Reproduction and validation

Build the candidate from the commit titled `Wake macOS readers through the
committed sequence word` in its own Cargo target directory. Preserve the previous library and use the exact harness snapshot
[bench.py](bench.py), with `--bindings bindings/python` when invoking the snapshot.
Use `--receipt identity --baseline-receipt identity` for numeric runs, `--binary`
for forwarding, and `--messages 150 --interval-ms 20 --repeats 5` for paced runs.
Pass the two library directories to `fanout.py` with `--bindings bindings/python`.
Compile the diagnostic with `cc -dynamiclib -std=gnu11 -O2 -Wall -Wextra` and use
`--native-counts` only for separate diagnostics. The diagnostic requires a host
with the public APIs; it does not support older-host execution.

The focused core tests cover publication before entering wait, timeout, header
mapping lifetime after Pool drop, three independent readers, cancellation,
concurrent writers, overwritten snapshots and fallback without notifications.
`just check` passes, including formatting, clippy, all Rust and integration
tests, harness checks and vendor integrity. `just bindings-test` passes: Go
packages, 51 Python tests (one existing skip), 51 Node tests, packaged secure
server start/restart, remote-only packaging and TypeScript checks.

## Earlier prototype and design checks

### Standalone correctness spike

[shared-wait-spike.c](shared-wait-spike.c) creates one mapped file and three child
processes. Each child opens the file independently and maps another address,
reads generation zero, and waits on its own alias. The parent publishes one
with an atomic release store and wakes all waiters through its mapping. All
three children return successfully and observe one. A separate wait times out,
and an already-changed expected value returns immediately. The prototype
unmaps memory, reaps its children and removes its temporary file.

The first untouched-alias experiment returned EFAULT before registering a
waiter. Reading the observed generation before the wait faults in the mapping
and follows the intended compare-and-wait protocol; the corrected spike succeeds.
The SDK documents transient EFAULT/EINTR and advises reading the current value
before retrying. Production integration must preserve this state check and its
existing best-effort fallback rather than assume every kernel wait succeeds.

[shared-wait-spike.raw.json](shared-wait-spike.raw.json) records the compiler,
platform, hashes, functional results and diagnostics. Its elapsed fields verify
timeout/changed-value behavior; they do not establish end-to-end latency,
throughput, CPU or a production performance gain. The spike requires macOS 14.4
or later. Build it with `cc -std=gnu11 -O2 -Wall -Wextra` and run the executable.

### Integration hypothesis

The pool already has a newest-sequence word. A native wait can compare that
word with the generation the cursor observed when it found no unread frame.
Sampling a newer generation after the empty check could hide an unread append,
so integration must retain the observed generation. The storage writer can
publish that word atomically in its existing little-endian location and wake
all waiters after releasing the lock. The cursor and owned snapshots remain
authoritative; a wake-up only asks them to recheck state.

Integration must guard API availability and retain the semaphore/polling route
for older operating systems or unavailable native waits. It must also preserve
deadlines and cancellation checks. Compare saturation, paced delivery, idle CPU,
writer notification cost and one-writer/three-reader fan-out before adopting it.
No helper thread, broker or new notification registry follows from this spike.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
