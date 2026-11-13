# Borrow the file through an append

Ordinary append now locks the pool's existing file through a borrowed guard.
The previous guard owned a duplicate descriptor, so each record duplicated a
handle and closed it after unlocking. Both descriptors referred to the same
open file description. The duplicate served the Rust borrowing structure,
rather than a different storage or locking requirement.

The storage mutation function now accepts the mapped bytes, header and path
that it uses. Those field borrows let the lock guard retain a borrow of the
file while the encoder updates the other fields. This removes the duplicate
and close with safe Rust. It adds no Arc, runtime ownership switch, alternate
commit path or public API. The public owned AppendLock still opens its own
independent description, and deletion uses that existing owned lock.

The append still locks exclusively, checks the pathname identity after locking,
refreshes the header, commits the frame and releases the lock before posting its
wake-up hint. The shared snapshot lock and stable owned frames remain unchanged.
Flush errors still report failure after any already-published state, matching
the original behavior. The guard unlocks on all returns.

## Matched complete pipeline measurements

Seven alternating optimized repeats compare the previous structural-sizing
library with this change on macOS 27.2, Apple M5 Pro and Python 3.15.2. Every
run uses 100,000 messages, separate producer and consumer processes, matching
Fast durability and enough history for all records. Each consumer checks every
sequence and complete binary payload or decoded data. Setup/imports stay outside
timing, and both processes finish before timing ends. Binary mode repeats one
pre-encoded record; JSON modes generate the same varying sensor records.

| Requested work | Baseline messages/s | Candidate messages/s | Combined CPU baseline/candidate s |
| --- | ---: | ---: | ---: |
| Full JSON producer receipt and complete consumer | 49,473 | 50,227 | 3.330567 / 3.284940 |
| Numeric producer receipt and complete consumer | 79,859 | 81,216 | 2.378782 / 2.336593 |
| Binary forwarding and raw consumer | 143,966 | 152,705 | 0.988155 / 0.956407 |

Binary throughput increases 6.1%, with 3.2% less combined CPU. Full and numeric
JSON paths gain 1.5% and 1.7% throughput; combined CPU falls 1.4% and 1.8%.
These modest ordinary-path gains fit the work removed. The full consumer still
pays its conversion and runtime costs. Do not infer that removing two operations
must produce a fixed gain in every application or platform.

[full.raw.json](full.raw.json), [identity.raw.json](identity.raw.json) and
[binary.raw.json](binary.raw.json) preserve all repeats, per-worker CPU, peak
resident memory, hashes and verification counts. Peak memory includes process
setup and mapped pages, rather than only the temporary append state. Candidate
builds use an isolated Cargo target directory. The baseline library hash matches
the earlier structural-sizing experiment; its native-call trace verifies the
old guard behavior. The candidate uses the same dependency lock and profile.

## Count the removed operation

The separate [native-call diagnostic](operations.raw.json) checks 10,000 records
per library. The old writer makes 10,000 descriptor closes during append; the
candidate makes zero. Both make 10,000 exclusive locks, unlocks, pathname stat
calls and semaphore posts, and open their semaphore once. Both make zero msync
calls in Fast mode. Source inspection also verifies that File::try_clone has
left the ordinary append path; the probe does not wrap variadic fcntl.

The diagnostic measures function-call counts, not uninstrumented performance.
Its timing must not serve as a speed result. The comparison therefore accounts
for the two removed descriptor operations separately from the remaining
pathname check, lock, publication and wake-up work. Resource ownership now
matches its required lifetime: the Pool keeps the file alive throughout append.

## Reproduction and checks

Build the candidate from the accompanying implementation patch against commit
`cb8342e` in its own `CARGO_TARGET_DIR`. Save its optimized library and compare
with the preceding library. [bench.py](bench.py) preserves the exact harness;
pass `--bindings` when running the saved copy outside the repository scripts
folder. Use 100,000 messages and seven repeats. Default mode requests full
receipts; use `--receipt identity --baseline-receipt identity` for numeric,
and `--binary` for opaque forwarding. Use `--native-counts` only for the separate
count diagnostic, with the macOS operation probe from the pipe evidence folder.

All 60 pool tests pass, including process contention, independent snapshots,
public guard exclusion, replacement after waiting for a lock, deletion,
sequence exhaustion, overwrite, flush failure and forced termination after a
flush acknowledgement. Formatting and clippy also pass. The full repository
check (`just check`) also passes.
