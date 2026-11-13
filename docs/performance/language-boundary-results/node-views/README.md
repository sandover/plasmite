# Node owned-view boundary measurements

The additive getView API retains immutable Lite3 bytes and reads fields through
Proxies. These results establish its costs; they do not claim that every view
operation beats the current JSON API.

## Method

Run `node --expose-gc bindings/node/scripts/bench_views.js 30 3 OUTPUT` with
matching optimized addon and libplasmite assets. The script verifies full data
and array sums, alternates lane order, records binary/JavaScript hashes and keeps
raw repeats. Measurements ran on the available macOS arm64 host with Node
26.13.0. Builds and competing measurements stopped during each short run.

Cold field lanes acquire a fresh message and request one number. Cold native
lanes request a complete native object. Warm lanes already own their input:
native-object controls isolate the extra cost of view access, while copy lanes
request independent containers from both inputs. Setup/JIT warmup sits outside
timed repeats. First acquisition latency appears separately as initialViewMs.

## Correcting the initial latency regression

The first bridge checked eleven dynamic-library symbols on every getView call.
A separate instrumented build attributed median 261.5 microseconds to that guard,
6.3 to worker queueing, 2.5 to the core read, 3.1 to resolve scheduling and 0.04
to external-memory accounting. The repeated capability check caused the fixed
latency floor. One-time ABI detection removes it while retaining the clear error
for an older library. The async API and storage policy stay the same.

| Workload | Initial view field µs | Cached-ABI view field µs | Current JSON field µs |
| --- | ---: | ---: | ---: |
| Small | 259.04 | 13.05 | 3.78 |
| Wide (256 extra fields) | 298.34 | 47.90 | 59.62 |
| Array (4,096 objects) | 630.60 | 426.56 | 408.05 |

These are medians of three 30-operation repeats, useful for direction rather
than narrow confidence claims. The cached guard removes 95% of small-message
view latency. The wide selected read takes about 20% less time than the current
JSON route; small and array-heavy cold reads still lose. First process getView
in the cached run took 0.557 ms, including ABI detection and first worker use.

Warm field access costs about 1.2 microseconds through the initial bridge versus
roughly 0.005–0.01 microseconds on an existing native object. View array iteration
and full native conversion also lose. Wide warm direct JSON formatting takes
20.61 versus 21.73 microseconds for native stringify, while the large-array JSON
case loses. The raw files include every lane; no primary-API cutover follows
from these measurements.

Current iteration retains a vector of borrowed native entry references, though
it creates no eager child proxies. Bounded native cursors and removal of scalar
descriptor objects remain follow-ups. Memory samples include GC/other-allocation
noise and cannot establish exact native-owner retention or prompt release.

## Evidence and checks

- [Initial repeats](initial.raw.json) and [cached-ABI repeats](cached-abi.raw.json).
- [Queue/core/resolve diagnostics](dispatch-diagnostic.txt) and
  [ABI/publication diagnostics](abi-diagnostic.txt). Diagnostics use a temporary
  instrumented addon; the timing runs above use uninstrumented builds.
- Six focused Proxy/native checks cover reflection, copies, exact integers,
  queued close and retained snapshots. All 63 Node tests pass; three socket tests
  require localhost UDP permissions outside the filesystem sandbox. TypeScript
  root/subtree/envelope inference and the export-surface checks pass.

New view entry points leave legacy methods available. Direct native conversion
still needs a measured implementation choice; fewer codec passes alone do not
justify its slower complete result.

## Returning scalars without descriptor objects

The first bridge allocated a temporary JavaScript `{ value: scalar }` for every
field, then immediately discarded it in the Proxy wrapper. The next bridge
returns primitives directly, uses undefined only for absence, and reserves node
descriptors for containers/bytes. Tests distinguish null, false, zero and empty
strings from absence.

Object presence now returns a boolean without extracting a scalar. Dense array
presence follows the known index/count and never calls the backend. This avoids
the extra lookup/decode that Array.map performs through HasProperty. A focused
check counts zero indexed reads for presence and exactly one get per map item.

A [matched alternating A/B](scalar-presence-ab.raw.json) rebuilds commit 86d4594
in an isolated package and compares it with the primitive/presence bridge. Both
variants load the same core library hash. Three pairs run AB/BA/AB, with 2,000
small-array operations and 100 wide/large-array operations per repeat.

| Warm operation | Small before/after µs | Wide before/after µs | 4,096 array before/after µs |
| --- | ---: | ---: | ---: |
| Field chain | 1.207 / 1.109 | 1.173 / 1.049 | 1.333 / 1.213 |
| Array iteration | 4.250 / 3.820 | 90.271 / 75.970 | 2,831.753 / 2,375.276 |
| Array.map | 5.762 / 3.766 | 149.274 / 86.783 | 4,737.057 / 2,829.438 |

The combined boundary changes reduce field time 8–11%, iteration time 10–16%,
and map time 35–42%. The isolated count check establishes why presence saves
work; these timings measure the combined changes rather than allocate the gain
between them. Cold-read latency has no established gain from this slice.

The earlier [short exploratory run](scalar-values.raw.json) used only six
small-array operations per repeat and showed a regression there. The longer
matched A/B resolves that noise. To repeat warm-only comparisons, set
PLASMITE_BENCH_WARM_ONLY=1; PLASMITE_BENCH_BINDING selects an isolated package
root. The script records the chosen package's addon/library/JS hashes.
