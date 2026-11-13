# Native shared-address wake-up investigation

The current Darwin reader polls a named semaphore every 5 ms inside each wait.
The installed SDK also provides a public primitive that can wait atomically on
an expected shared-memory value and wake all processes waiting on that value.
The SDK declares these APIs from macOS 14.4 in
`usr/include/os/os_sync_wait_on_address.h`. The SHARED flags explicitly support
multiple processes mapping a shared region. This investigation tests that
primitive against Plasmite's need: wait until the committed pool generation
changes, then let each independent cursor inspect its own position.

## Standalone correctness spike

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

## Integration hypothesis

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
