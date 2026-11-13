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
