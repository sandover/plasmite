# Go owned-view stream checkpoint

Local and UDP readers transfer one checked native snapshot into aligned private
Go bytes. A narrow internal helper exports data/meta offsets and the duplicate
container proof; Go copies/sorts those offsets without another validation walk.
Delivered messages contain Go-managed data and require no Close. The existing Go view
reader then borrows those bytes for typed/selected access. Native reception and
the transfer to Go each copy payload bytes. This change removes eager JSON/native
trees and repeated validation.

Whole-message `AppendView(message, nil)` borrows the private Go bytes for one
synchronous commit and returns fresh identity over the same immutable backing.
Explicit nonnil tags request retagging; `AppendView(message.Data(), nil)` remains
the native subtree-copy control. Independent mutable exports still copy.

The APIs reuse native tail/follow state for ordering, gaps, matching limits,
skips, progress and UDP duplicates/late packets. Go retains bounded idle waits,
contexts, serialized native reads and explicit channel backpressure. TailViews
defaults to an unbuffered channel; a positive Buffer opts into a bounded queue,
plus one message awaiting delivery. The pull stream has no background reader. A focused
idle test caught an existing UDP deadline race: the deadline could pass before
`ctx.Err()` became nonnil, causing `(nil, nil)`. The shared loop now reports
DeadlineExceeded in that interval.

## Frozen artifacts and coverage

[The manifest](manifest.json) identifies both fixed executables and their shared
release library in `tmp/go-owned-stream-checkpoint`. The source overlay (outside Git)
contains every compiled Rust/C/Go module, including internal headers
and the raw-follow implementation. Apply it over the recorded base commit, which
supplies vendor sources. Loaded-library probes confirm both executables resolve
the private release library. The final executables use the archived runtime and benchmark source, and the
library uses matching Rust/C inputs.

Race checks, strict `cgocheck2`, Go1.22 compatibility, four focused Rust tests and
all-target Clippy pass. The real CLI UDP SDK test and scripted lifecycle tests
pass. Ninety three-record release smoke lanes verify complete converted values,
raw bytes and forwarded results across small, wide, long-string, array and
native-byte fixtures. Smoke timings carry no performance claim; central paired
measurements remain pending.

## Central measurement commands

From the repository root, use one fixed executable per process:

```sh
DYLD_LIBRARY_PATH="$PWD/tmp/go-owned-stream-checkpoint/lib" \
  tmp/go-owned-stream-checkpoint/local.test -test.run='^$' \
  -test.bench=BenchmarkOwnedStreams -test.benchtime=2000x -test.count=1
DYLD_LIBRARY_PATH="$PWD/tmp/go-owned-stream-checkpoint/lib" \
  tmp/go-owned-stream-checkpoint/udp.test -test.run='^$' \
  -test.bench=BenchmarkOwnedUDP -test.benchtime=2000x -test.count=1
```

Alternate `PLASMITE_GO_REVERSE=0/1` across repeated processes and retain raw
output plus artifact hashes. Each fixture supplies raw reads, view/JSON selected
and full typed reads, raw/view/JSON forwarding and the explicit view subtree-copy
control. Every read checks its sequence; complete full results and final
forwarded data receive independent checks outside timing. Raw and whole-view
forwarding also verify exact stored bytes. JSON byte controls store base64
strings while producing the same typed byte output; they cannot preserve the
native byte kind.

Local sources contain 128 prefilled records. Reopening after each 128 records
bounds fixture size and includes amortized stream setup in the measured work.
UDP uses one DATA in flight and a test-only Go channel acknowledgement, so its
measurements include loopback and pacing overhead, not saturated packet rates.
The mock peer sends direct opening PROGRESS outside timing. Both harnesses report
Go allocations and cgo calls; native allocations require a separate probe.


## Retained-child wrapper reassignment

Children now copy the immutable encoded/proof slice headers into Value. Their
node offsets stay bound to those buffers when the caller reassigns or clears
`*MessageView`. This adds no per-message heap owner and copies no payload bytes.
Container/scalar/byte/array children, wrapper aliases, duplicate-key proof and
source-Value forwarding all pass reassignment tests after forced collection.

The pre-fix executables and library above remain unchanged. The candidate local
executable lives in `tmp/go-child-checkpoint/local.test` and uses that same frozen
library. Existing complete point/typed/selected/full-array benchmark lanes remain
the comparison harness; only two benchmark composite literals changed to name
the embedded snapshot header. Focused allocation diagnostics keep acquisition
at two allocations for small records and three for the array sizing retry.
Warm typed and full-array allocation counts stay unchanged. Two returned selected
Values allocate 80 more bytes in total, with the same allocation/cgo-call counts:
each Value replaces an eight-byte mutable-wrapper pointer with 48 bytes of stable
slice headers. Uncontrolled diagnostic times support no timing claim.

[Candidate source/artifact hashes](child-manifest.json), the complete source
overlay (outside Git), and [allocation diagnostics](child-allocation-probes.json)
preserve this comparison. Use `BenchmarkOwnedViews` for point/typed/selected/full
array controls, and the existing stream benchmarks for complete delivery paths.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
