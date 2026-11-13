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

The initial bridge retained a vector of borrowed native entry references and
created scalar descriptors. Later sections measure removal of those costs.
Memory samples include GC/other-allocation noise and cannot establish exact
native-owner retention or prompt release.

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

## Bounded sequential iteration

The next adapter fills sixteen inline C entry slots from the core's retained
sequential cursor. It no longer allocates an entry vector proportional to the
whole array before returning its first value. The inline slots occupy 512 bytes
on this host; the core cursor also keeps bounded state and an owner reference.
Explicit key enumeration/full conversion can still allocate their requested
complete results.

The [matched cursor A/B](bounded-cursor-ab.raw.json) rebuilds adapter commit
036aed2 against the same core library as the new adapter and alternates three
pairs. Taking only three elements from 128-element arrays falls from 5.45 to
3.36 microseconds; from 4,096-element arrays it falls from 50.10 to 3.76
microseconds (92.5% less time). Full scans show 7% less small-array time and
2% less wide-array time, while the large scan rises 1.5%; no large full-scan
speed improvement follows. Map uses indexed access and stays within roughly
3% across variants. Warm native-object iteration remains much faster.

The benefit is bounded scratch storage and early termination without scanning
all entries. GC/RSS samples remain observational, not exact ownership counters.

## Defining native object properties in one call

The direct walker previously called napi_define_properties separately for every
field. napi-rs allocates a descriptor vector on each call. The next walker builds
one property list per multi-field object and calls the stable bulk API once;
single-field objects keep the allocation-light path. Keys own their strings and
JS values remain rooted until definition. Own-property semantics and ordinary
writable/enumerable/configurable attributes stay intact, including __proto__.

The [matched property-batch A/B](batch-properties-ab.raw.json) uses adapter
0b7ae4d as its rebuilt baseline and the same core library hash for both variants.
Three alternating pairs show small native conversion 4.26→3.78 microseconds and
wide conversion 106.56→89.02 microseconds (16.5% less time). The array-of-single-
field-objects case stays roughly unchanged, 1,256→1,272 microseconds.

A benchmark-only JSON.parse(toJson(data)) control on the verified safe-number,
no-byte fixtures takes about 2.03/42.80/297.81 microseconds for small/wide/array.
It still beats the direct walker. That control changes key order to canonical
JSON order and cannot preserve large i64/byte native types in general, so it is
not an automatic production fallback. Exact bigint and native byte conversion
checks still exercise the direct walker.

The [stable napi_define_properties API](https://nodejs.org/api/n-api.html#napi_define_properties)
works at the Node 24 floor. The newer
[node_api_create_object_with_properties](https://nodejs.org/download/release/latest-v24.x/docs/api/n-api.html#node_api_create_object_with_properties)
starts at 24.12 and remains experimental; this change does not require it.

## Reusing fixed root descriptors lazily

Repeated message.data/meta access previously looked up the same immutable root
member and allocated its descriptor before finding the existing weak proxy.
The factory now resolves the root and these two descriptors lazily, once per
message. It retains descriptors, not child proxies; identity-only use does no
root lookup. This is fixed message metadata rather than a general key cache.

The [JS-only alternating A/B](root-descriptors-ab.raw.json) uses the same addon
and core hashes for both variants, against frontend commit 6fe188f. Warm field
chains fall 1.188→0.728 microseconds (small), 1.007→0.634 (wide), and 1.102→0.663
(array), a 37–40% reduction. Small warm JSON falls 1.311→0.862 microseconds;
large JSON stays unchanged. Cold field results remain within 1%; identity-only
cold results vary across shapes, so they establish no broad latency improvement.
A count-based check verifies zero root calls before data access and exactly one
lookup each for repeated data/meta reads.


## Compatibility costs

Closed pools and a loaded library without the additive view ABI now preserve
`Usage` error metadata. Full-message export uses the core RFC3339 convention:
trim trailing fractional zeros and omit an empty fraction. Focused tests cover
the full unsigned timestamp range and both synchronous and asynchronous closed
pool calls.

The [matched contract benchmark](contract-fixes-ab.raw.json) compares fb0b029
with these fixes using identical core library hashes. Canonical timestamp text
costs 0.459→0.490 microseconds per call. Adding `kind` to closed-pool errors
requires a JavaScript Error object before the exception crosses the binding;
that error-only path costs 1.332→2.801 microseconds synchronously and
1.563→2.777 microseconds through getView. Successful reads take the same body.
Reproduce with `node bindings/node/scripts/bench_view_contract.js BINDING_PATH`;
the script reports seven repeats and asset hashes. The evidence alternates two
baseline/candidate pairs.


## Native copies: remove callback bookkeeping, then use the VM parser

The napi 2.16.17 `JsObject::define_properties` helper creates closure metadata
and an object finalizer even for plain data properties. Its descriptor iterator
also calls `Property::raw` twice. The direct walker now passes raw data-only
Node-API descriptors with null callback data. Keys and value handles stay live
through that one call; properties remain writable, enumerable and configurable,
including `__proto__`.

The VM parser still constructs requested native results faster. For a snapshot
whose existing validation pass proves safe integers, finite numbers and no
bytes, materialization formats the requested subtree in stored key order and
calls the captured `JSON.parse` builtin. Unsafe or unknown owners use the
corrected direct walker. This adds no validation pass or public API choice.
The C buffer goes directly into a copied VM UTF8 string and frees before parsing;
no intermediate Rust Vec/String copy sits between those steps. Ordinary view
reads keep Lite3 as their representation.

The [three-way comparison](raw-vs-json-copy.raw.json) uses three rotating orders
and identical core library SHA256
`ea3e5ab7c5bb262a86fcd88140c38a332074157d023fb084a28e0a004f798984`.
The baseline adapter comes from 611065c; core capability 4c686cb and formatter
37eabc9 supply the shared implementation. Median microseconds per native copy:

| Fixture | napi helper | Raw descriptors | Safe JSON parser |
| --- | ---: | ---: | ---: |
| Small | 2.664 | 2.135 | 1.331 |
| Wide | 93.10 | 65.49 | 45.09 |
| 4,096 objects | 1,296.8 | 884.8 | 343.3 |

Raw descriptors remove 20–32% of direct-walker time. The parser then removes
38%, 31% and 61% against that corrected walker. Exact-i64 fallback improves
0.771→0.556 microseconds through the raw descriptor change. Tests verify stored
key order, numeric keys, negative zero, escapes, dangerous names, safe integer
boundaries, native bytes and exact i64 fallback. A private callback spy verifies
which branch runs. Older view-capable libraries fall back through a separate,
once-only optional symbol check; Unix and Windows resolve optional functions
without hard imports.

The first small copy costs about 150 microseconds versus 79 for the old adapter,
including optional symbol discovery. Later copies use the cached function
pointers. Observed retained-copy heap after GC stays near 258 KiB for both direct
walkers versus 159 KiB for the parser on 4,096 objects. These samples suggest a
VM construction/layout benefit; they do not measure exact allocations or peak
native memory. The formatter temporarily creates encoded text for this explicit
copy operation.

Reproduce with `bench_views.js` and identical optimized libraries for each
adapter. [This patch](raw-copy-variant.patch) turns the current adapter into the
raw-only experimental variant; the baseline is 611065c. The evidence records
script, frontend, addon and shared-library hashes, first-use times, exact-value
fallback timings and copy-retention memory samples. The production choice keeps
the parser for safe owners and raw descriptors for every fallback.


## Native input and committed receipts

`appendView` evaluates JavaScript input once, builds Lite3 and queues its owned
encoding for storage. A private retained target separates input callbacks from
the original Pool: getters, tag iterators and serializers may close that Pool
without invalidating the append. Failed preparation discards the target. Native
bytes snapshot synchronously; shared backing requires an ordinary caller copy.
VM exceptions stay pending across native unwinding, preserving primitive thrown
values and avoiding napi's exception-to-string coercion.

An unchanged message view forwards its encoded snapshot. A root-view copy or
retag captures source ownership and tags, then performs the checked native copy
on the worker. Mixed JavaScript objects evaluate on the environment thread and
copy checked nested views through the shared writer. Destination builder handles
remain opaque; source ValueRef offsets identify checked native values.

Forwarded owners share one backing allocation charge. Each keeps a flattened
reference to the original accounting owner, and asynchronous jobs retain that
reference through their environment-thread finally hook. The optional
`plsm_view_retained_bytes` getter reports allocated payload capacity; older
libraries fall back to encoded length. This excludes owner/index metadata and
keeps the growing allocation rather than shrinking through another payload copy.
For example, the 64-KiB text fixture encodes 66,300 bytes but retains 131,072;
the 4,096-object fixture encodes 589,952 and retains 1,048,576.

The [complete producer run](native-input.raw.json) and
[matched scope experiment](input-scope-batching-ab.raw.json) compare native input
with optimized stringify + shared C parsing, both returning the same owned-view
interface. The latter uses the same core SHA256
`40692aa657a709f8636eebb01c167298944a5725f40bc46be3edbb7d22512c16`.
Its final variant records these median microseconds:

| Append and request one field | Native input | Stringify + C parse |
| --- | ---: | ---: |
| Small | 22.14 | 21.21 |
| Wide | 144.28 | 74.91 |
| 64-KiB text | 41.80 | 74.81 |
| 4,096 objects | 1,474.34 | 444.34 |

Native input saves 44% on long text. Many small objects remain slower through
the Node-API traversal; this checkpoint does not claim a universal encoding win.
Separate-handle append/consumer field reads show the same split: text 57.90
versus 97.34 microseconds; arrays 1,676.77 versus 618.45. These consumers run in
the same process and do not measure cross-process wakeups.

| Existing view and request one field | Native reuse/copy | Explicit JSON conversion |
| --- | ---: | ---: |
| Forward wide | 24.66 | 81.78 |
| Forward 64-KiB text | 16.93 | 74.92 |
| Forward 4,096 objects | 31.05 | 585.33 |
| Retag 64-KiB text | 30.36 | 79.97 |
| Retag 4,096 objects | 550.04 | 559.48 |

The forwarding gain comes from reusing the immutable payload rather than
formatting, parsing and rebuilding it. Retagging currently uses a full checked
native copy. The scripts also record checked subtree copies, full native
materialization, exact i64 input, native bytes, process CPU, GC, event-loop delay
and retained forwarding samples. Byte input has no equivalent JSON-native byte
type, so that lane makes no cross-contract speed claim. Event-loop sampling has
1-ms resolution; heap/RSS samples do not establish exact allocation or peak
memory bounds.

### Scope allocations inside the runtime

[Node's handle-scope implementation](https://github.com/nodejs/node/blob/v26.13.0/src/js_native_api_v8.cc#L2685)
allocates a C++ wrapper on open and deletes it on close. Per-value scopes
therefore added a heap allocation for every visited property. The encoder now
opens one scope across 16 siblings and skips a scope for single-child containers;
branching containers bound temporary handles, and linear chains have the depth
bound. Ancestor handles stay in outer scopes.

A [diagnostic-only counter](input-scope-allocations.raw.json) measures those
specific scope allocation requests, not total VM/Rust allocations:

| Fixture | Per value | Batched |
| --- | ---: | ---: |
| Small | 11 | 2 |
| Wide | 515 | 25 |
| 64-KiB text | 6 | 1 |
| 4,096 objects | 8,195 | 257 |

The matched uninstrumented runs improve array append/field 1,622.62→1,474.34
microseconds (9.1%), wide 155.38→144.28 (7.1%), and small 24.63→22.14 (10.1%).
The allocation reduction is much larger than the time reduction, which points
the remaining investigation toward key extraction, helper calls and VM access.
[The per-value patch](per-value-scopes.patch) reconstructs the baseline;
[the counter patch](scope-diagnostics.patch) adds the optional diagnostic export
used by `count_input_scopes.js`. Production code contains no counter.

### Improve the JSON control too

The shared parsed-JSON copier now computes the native-safe property during its
existing walk. The [same-addon, different-library experiment](json-safety-result-copy-ab.raw.json)
verifies parser callback counts 0→1 for safe values and zero for exact large i64.
Warm native materialization falls 66.14→47.03 microseconds on wide data and
923.08→365.33 on 4,096 objects. This prevents an unknown safety flag from
penalizing JSON-produced receipts in the producer comparison. Shared changes
come from d929bc2 (safety/capacity) and 511ce29 (short keys); the raw files retain
the precise library hashes.

The [existing-read comparison](producer-read-regression-ab.raw.json) uses adapter
289fe99 and the same frozen library. Small direct JSON export improves
0.808→0.543 microseconds by copying formatter bytes straight into the VM string.
Other read lanes show mixed changes: selected large-array reads rise about 7%
and warm array-field reads about 12%, while selected wide reads fall about 7%.
The short cold-identity samples vary more. These remain explicit follow-up
measurements rather than a blanket read-performance claim.

Reproduce with `bench_input_views.js`, `bench_json_result_copy.js` and
`bench_views.js`. Run isolated library variants in separate processes: a process
can reuse an already-loaded libplasmite dependency. The producer script records
actual native, optimized JSON and legacy Message controls; legacy Message also
decodes its complete result synchronously and has a different interface cost.
