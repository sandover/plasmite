# Portable checked views

The remote-only Node backend keeps Lite3 as its immutable record and uses the
same Rust/C checker, traversal, JSON formatting and input builder as native
bindings. Its packaged module has 114,270 bytes (about 44 KiB gzip), no runtime
imports and no native addon requirement. The existing Proxy implementation
supplies properties, arrays, reflection and readonly byte views.

[Core design and build instructions](../../../../bindings/node/portable-core/README.md)
explain the copy and ownership boundaries. The module starts with roughly 1.1 MiB
of linear memory, including its stack; the allocator grows and reuses it.
The host loader targets Node 24+, which the focused tests exercise directly.
Compilation happens lazily and synchronously, so capture finishes before its
Promise returns. Neither cold initialization nor buffer growth reruns input
getters or builds an intermediate native capture tree.

## Checked scope

The 12 focused/package cases cover natural properties, arrays, exact i64 values,
negative zero, native bytes, malicious names, immutable children, malformed
indexes, valid hash collisions/duplicates, native-generated input and memory
growth during iteration. They also cover JSON hook/Proxy order, mutable input
snapshotting, exceptions, cycles/depth limits, reentrant encoding, omitted versus
explicit tags, full/subtree forwarding, malformed-key hook order and empty
frames. A packed tarball installs and works with no native artifacts.
The strict Wasm Clippy check and artifact/shared-source verifier pass.

The [operation preflight](native-operations-verified.json) checks portable, native
and JSON controls on small, 200-field, 64 KiB-string and 4,096-element array fixtures.
Its timings are null. The native control loads the supplied addon and verifies
the actual loaded library against the requested hash. A mismatched library fails
rather than yielding an unattributed comparison.

The [startup preflight](native-startup-verified.json) uses a fresh Node process.
Its null timing fields verify the cold path without supplying a performance
claim. Portable timing comes from the central pass using the commands below;
the preflights do not supply throughput or latency results.

## Central results

The [raw operation runs](central/operations.json), [fresh-process startup runs](central/startup.json) and [retention diagnostic](central/retention.json) come from the centralized fixed-artifact pass on macOS arm64, Node 26.13.0. The [summary](central/summary.json) reports medians from seven repeats, with ranges and CPU costs for every operation lane. Every result check passed; the native control verified its loaded library hash.

Portable views provide the requested semantics without an addon. They do not offer a general speed gain over JSON. Sparse access avoids materializing a native tree, but acquisition still copies and checks the complete encoded buffer. Full copies and array scans cross the Wasm boundary repeatedly or format and parse the requested data.

Median wall time, microseconds per operation:

| Fixture | Portable cold sparse | Native cold sparse | JSON parse + sparse | Portable cold full | Native cold full | JSON parse + full |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Small | 3.11 | 3.37 | 0.52 | 3.33 | 3.31 | 0.52 |
| 200-field | 33.33 | 20.10 | 24.89 | 99.61 | 66.30 | 25.94 |
| 64 KiB string | 8.58 | 9.52 | 16.46 | 44.37 | 44.25 | 16.09 |
| 4,096 integers | 35.22 | 33.23 | 18.57 | 163.45 | 99.85 | 17.93 |

The long-string sparse case takes about 48% less time than JSON parsing because it reads selected fields without decoding the large unused string. The wide and array fixtures show the cost of checking larger Lite3 layouts: their encoded sizes are 35,486 and 169,439 bytes, versus 8,343 and 19,516 JSON bytes. Warm portable sparse reads cost 0.45–0.51 microseconds; already-native property access costs 0.07–0.09. Scanning all 4,096 integers costs 451 microseconds portable, 329 native-view and 1.27 on an already-built native array. Construction stays outside that last control.

Portable encoded production costs 2.52 / 111.67 / 12.32 / 228.17 microseconds for small / wide / long-string / array fixtures. JSON request construction costs 0.29 / 6.24 / 22.24 / 13.83. These produce different physical formats; the numbers do not establish an interchangeable codec ratio. The native addon exposes no storage-free encoder control. Complete delivery measurements belong to the HTTP and native-input evidence.

Fresh-process portable require, first decode and first sparse access cost 0.613, 0.980 and 0.183 milliseconds respectively (separate medians). Native require, first decode and first sparse cost 1.942, 0.114 and 0.023 milliseconds. Process startup stays outside those measurements. The module adds 114,270 bytes, or 45,132 gzip bytes; no download latency is inferred from size.

The central retention run confirms one child retains 66,076 backing bytes and releases them after collection. Fifty more acquisitions return live owner count and backing bytes to zero. Linear memory grows from 1,507,328 to 4,521,984 bytes and retains that allocation for reuse.

These runs identify historical Wasm SHA `96de040ab05a975eb2168e4777562edbe25c229ca1574303e11b50f799d5f6eb` and adapter SHA `3e0f1c9e82275f3f0df0d6e5e40a70d5876722324009ee792113731d41a1017f`; the existing source archive matches them. The native baseline library SHA is `6a08cb2f48220adfbc3894d5108c097cd7ed60eb8300636f4a4f163f8b9280cc`. Later package rebuilds may update shared-source manifests; these raw runs continue to describe this measured checkpoint.

## Retention

The [explicit-GC diagnostic](retention.raw.json) has null performance timings.
A selected child keeps one 66,076-byte backing snapshot after its parent wrapper
goes away. Dropping that child releases the owner: owner count and retained
backing bytes both reach zero. Fifty more acquisitions then release their owners
too. Linear memory stays at its high-water allocation and reuses freed space;
it does not shrink when an owner disappears.

`backingBytes` counts live owner payload capacities. It excludes object metadata,
duplicate maps, allocator overhead, workspaces and JavaScript wrappers. Linear
memory length includes the whole reserved page buffer and is not a process-RSS
measurement. Finalization depends on garbage collection; applications must not
expect immediate reclamation after the last source-level reference disappears.
Encoded-only producers release their temporary owners without waiting for GC.
All mutable buffers exported to JavaScript own a copy.

## Central reproduction

The harness is [bench_portable_views.js](../../../../bindings/node/scripts/bench_portable_views.js).
[Checkpoint hashes](checkpoint.json) and the exact source archive (outside Git)
identify the implementation. The maintained package also carries the artifact's
compiler and shared-source manifest.

```sh
node bindings/node/scripts/bench_portable_views.js \
  --mode operations --repeats 5 --output /tmp/portable-operations.json
node bindings/node/scripts/bench_portable_views.js \
  --mode startup --repeats 7 --output /tmp/portable-startup.json
node --expose-gc bindings/node/scripts/bench_portable_views.js \
  --mode retention --output /tmp/portable-retention.json
```

Pass `--native-addon /path/index.node --library-file /path/libplasmite.dylib`
for native read controls. The addon must provide checked `nativeViewFromBytes`;
missing native support stays explicit. Its loaded library must match the given
file, even when its embedded loader paths choose a different location.
Use `--verify-only` to run each result check once with null timing fields.

Cold lanes receive already available bytes and include acquisition plus requested
work; they exclude HTTP/storage. Warm native-object controls exclude construction.
The producer JSON control constructs a different request format, so report its
cost rather than claiming interchangeable encoding work. The native addon has no
storage-free encoder export; native complete producer comparisons belong to the
HTTP and captured-input delivery harnesses. No eager objects substitute for
unsupported views.

The standalone Wasm byte/gzip sizes describe the added artifact payload. They do
not measure a real network download or claim that the existing multi-platform
npm package shrinks to that size. The backend supports Node remote clients;
it does not promise a browser bundle or native local storage without an addon.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
