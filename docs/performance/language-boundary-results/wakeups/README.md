# Local follower wake-ups

Shared-address waits cut paced delivery latency and idle CPU on macOS 14.4
and later. The writer wakes every independent reader after publishing a new
sequence. This replaces 5 ms semaphore polling and adds a kernel wake per append;
binary bulk throughput falls 8.1% in the matched run below. The installed SDK
declares the public APIs in `usr/include/os/os_sync_wait_on_address.h`.

## Wake-all on Linux

An idle reader arms a shared bit. The next writer clears the bit when it
publishes the new sequence and wakes every waiting reader when the bit was
set. Writers skip the wake call when the bit is clear. Readers still check
the pool after one second so a writer that exits after committing cannot
leave a reader asleep indefinitely. The C reader can wake immediately on
cancellation on kernels with `futex_waitv`.

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

## Adopted macOS path

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
slices. Unavailable or failed hints retain bounded polling. Other Unix hosts without shared-address waits use 5 ms semaphore polling.
Windows checks for changes on a timer.

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
