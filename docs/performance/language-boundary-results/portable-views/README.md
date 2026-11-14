# Portable checked views

The remote-only Node backend keeps Lite3 as its immutable record and uses the
same Rust/C checker, traversal, JSON formatting and input builder as native
bindings. Its packaged module has 114,270 bytes (about45 KiB gzip), no runtime
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
