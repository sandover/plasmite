# Go stream ownership and performance

Local and UDP streams return independent immutable snapshots in Go memory.
Whole-message forwarding reuses encoded data and assigns destination identity;
subtree forwarding builds a new encoding. Retaining children keeps their source
buffers alive even when the caller reassigns the MessageView wrapper.

Use `BenchmarkOwnedViews`, `BenchmarkOwnedStreams` and `BenchmarkOwnedUDP` in
the Go binding with fixed libraries and alternating lane order. Go allocation
counts exclude native allocations. UDP runs below use loopback and explicit
one-packet pacing; they measure complete delivery rather than saturated traffic.

## Central retained-child comparison

[Seven paired processes](central-runs/manifest.json) compare the original stream
executable with the child-fixed executable using the same release library. Each
runs 2,000 iterations per lane. [The summary](central-runs/summary.json) retains
all 65 point/typed/selected/full-conversion comparisons and their controls; raw
runs retain every sample. Both variants pass all output checks.

The fix keeps allocation counts and cgo calls unchanged in every compared lane.
Small acquisition still allocates two Go objects/1,104 bytes; array acquisition
still allocates three/50,256 bytes including its initial sizing buffer. Typed
decoding and full conversion add no new allocation. The two returned selected
Values use 293 bytes per warm read versus 213: 80 additional metadata bytes.
Typed Decode builds no returned Value slice, so it avoids that metadata growth.

| Warm selected read | Before | Child-fixed | Change |
| --- | ---: | ---: | ---: |
| Small | 135.8 ns | 144.2 ns | +6.2% |
| Wide Unicode keys | 150.3 ns | 159.1 ns | +5.9% |
| Wide ASCII keys | 154.8 ns | 160.0 ns | +3.4% |
| Unrequested array | 135.9 ns | 146.1 ns | +7.5% |
| Long selected string | 5.228 µs | 5.135 µs | −1.8% |

Stable child lifetimes cost about 5–10 ns in the short warm selected lanes and
40 metadata bytes per returned Value. They copy slice headers and retain the
original buffers; they add no per-message heap owner or payload copy. Cold
selected reads range from −1.9% to +2.1%. Warm typed small/array reads move
208.6→214.2 ns and 242.9→243.0 ns. Full array conversion moves 72.73→72.98 µs;
its cached JSON control moves 62.23→63.48 µs. The full view conversion therefore
still takes about 15% more time than that cached-JSON control.

Long-string acquisition rises 4.0%, with exact-sizing/native-copy controls rising
2.3%/2.4%; long cold typed reads rise 2.9%. The summary preserves these changes
and the independent controls. This correctness fix does not establish a general
read speedup. It prevents legal single-thread wrapper reassignment from pairing
a child's offset with another message's bytes.

## Central stream consumption and forwarding

Three complete processes per transport run 500 iterations per lane, alternating
fixture/lane order. They use the child-fixed local/UDP executables and the same
stream library. These are current route comparisons, not before/after claims
against a prior stream implementation. Setup/verification stays outside timing;
local reads include amortized reopening every 128 records. UDP includes loopback
and the test-only one-packet-in-flight channel pacing.

Local median consumption times in microseconds:

| Fixture | View selected | JSON selected | View full | JSON full |
| --- | ---: | ---: | ---: | ---: |
| Small | 1.41 | 2.00 | 1.77 | 2.10 |
| Wide | 5.64 | 17.58 | 37.73 | 39.97 |
| Long text | 6.93 | 52.50 | 36.87 | 53.70 |
| Array | 14.88 | 36.37 | 95.30 | 91.14 |
| Native bytes | 5.01 | 71.20 | 60.37 | 81.40 |

Selected view reads take 30–93% less time than the matching JSON routes. The
full array route takes 4.6% more time and allocates about 104.7 thousand Go bytes
versus 59.8 thousand for JSON. Full conversion explicitly builds the requested
Go graph; it does not gain the sparse-read benefit. The view route also copies
the larger Lite3 representation into Go before conversion. Full generic Decode
uses the documented JSON-compatible fallback; this table makes no direct-native
map-decoder claim.

Local median forwarding times in microseconds:

| Fixture | Raw read | Raw forward | Whole view forward | View subtree copy | JSON forward |
| --- | ---: | ---: | ---: | ---: | ---: |
| Small | 0.97 | 3.63 | 3.81 | 4.37 | 5.17 |
| Wide | 2.27 | 9.86 | 11.80 | 20.90 | 27.89 |
| Long text | 4.47 | 18.66 | 18.91 | 22.86 | 60.36 |
| Array | 6.62 | 16.71 | 18.60 | 57.02 | 69.82 |
| Native bytes | 4.25 | 31.86 | 15.04 | 21.59 | 79.67 |

Whole-view forwarding takes 13–67% less time than the explicit native subtree
copy. It keeps source payload/proof storage and assigns destination identity
without reconstructing the data. It remains 5%/20%/11% slower than raw forwarding
for small/wide/array records; raw readers perform less owned-view bookkeeping.
The result supports the whole-message shortcut without treating it as the
fastest route for every record. Whole-view forwarding uses four Go allocations
per local record versus six for raw forwarding and seven for subtree copy.

UDP median consumption/forwarding times in microseconds:

| Fixture | View selected | JSON selected | View full | JSON full | Whole view forward | View subtree copy |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Small | 22.13 | 20.02 | 18.01 | 20.79 | 24.05 | 24.21 |
| Wide | 21.26 | 59.16 | 59.39 | 81.61 | 31.00 | 42.23 |
| Long text | 21.22 | 90.11 | 58.65 | 90.44 | 33.21 | 40.51 |
| Array | 31.94 | 84.67 | 109.76 | 140.15 | 37.65 | 79.39 |
| Native bytes | 22.84 | 146.46 | 84.92 | 154.93 | 31.74 | 36.40 |

Small UDP selected views take 10.5% more time than JSON in these three repeats,
with samples ranging 17.42–27.81 µs versus JSON's 18.49–20.76 µs. Whole-view small
UDP forwarding moves less than 1% against subtree copy. Larger selected records
take 62–84% less time and larger whole-view forwarding takes 13–53% less than
subtree copy, while the complete raw controls remain in the summary. Scheduler,
loopback and pacing costs limit the small-record comparison; these are not
packet-loss or saturated-throughput results.

Native-byte fixtures use a 32,000-byte value. View/raw forwarding preserves its
native byte kind and exact stored payload. JSON controls store base64 text while
producing the same requested typed byte output. Go heap counters exclude native
allocations. Together with the complete raw runs, these boundaries keep sparse
reads, full conversion, forwarding and transport costs distinct.
