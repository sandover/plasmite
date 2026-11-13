# Native Node buffer ownership

Transfer owned Lite3 snapshots into Node Buffers. Keep the compact copy for JSON
results. This choice improves large raw reads and preserves ordinary JSON memory
behavior on the measured host.

The baseline copies the C ABI allocation into a Rust Vec, frees the original,
then transfers the Vec to Node. The adopted raw path transfers the original
allocation and frees it through `plsm_buf_free` when Node collects its Buffer.
It returns the JavaScript handle directly, avoiding a temporary Rust buffer
reference and its create/delete-reference calls. Async reads retain a movable
native owner until the environment thread creates the Buffer. Unsupported
external-buffer hosts receive a copy.

## Release measurements

Five alternating repeats compare the baseline addon with the candidate using
the same optimized native library, dependency lock and Node 26.13.0 on macOS
27.2, Apple M5 Pro. The workload repeatedly reads a stored record through the
complete native-to-JavaScript point-read path and verifies every returned value
or payload. Timings include handle close and forced garbage collection; setup
and 1,000 warmup reads sit outside timing. They do not measure a concurrent
producer or a network transport.

| Consumer | Records/run | Baseline median ms | Candidate median ms | Baseline/candidate CPU ms |
| --- | ---: | ---: | ---: | ---: |
| Small raw Lite3 | 20,000 | 60.926 | 56.039 | 70.624 / 65.567 |
| Large raw Lite3 | 5,000 | 67.353 | 49.338 | 82.981 / 65.430 |
| Small complete JSON object | 20,000 | 406.943 | 409.284 | 426.554 / 428.979 |
| Large complete JSON object | 5,000 | 440.922 | 437.733 | 456.330 / 452.802 |

The large raw consumer takes 26.7% less time and 21.2% less CPU. Small raw
results vary more; inspect all samples rather than assuming a fixed gain.
Complete-object results show no material timing change. Post-GC resident memory
for complete objects stays near the baseline: small 32.39/32.41 MiB and large
495.41/495.28 MiB. These are resident-memory samples, not live-object sizes or
peak-memory claims. The [small](release.raw.json) and [large](release-large.raw.json)
runs include CPU, verification counts and memory fields.

## Why JSON keeps a copy

An all-external prototype showed no complete-object speed gain and raised small
record post-GC resident memory from about 32 to 57 MiB. Live external memory and
heap usage matched after collection, so the difference concerns allocator
retention rather than leaked live buffers. A separate `malloc_size` diagnostic
found a 2,396-byte serialized result occupying 4,096 native bytes; a fresh compact
allocation occupied 2,560. At 66,908 bytes both occupied 81,920. The copy can
compact the original JSON serializer's allocation on this host. This diagnostic
does not establish other allocators' size classes.

The rejected [all-external small](release-all-external.raw.json) and
[large](release-large-all-external.raw.json) runs preserve that comparison.
[An earlier reference-wrapper prototype](release-ref-prototype.raw.json) and
[debug runs](debug.raw.json) supplied diagnostics, not the adoption result.

## Reproduction and limits

[bench.js](bench.js) accepts baseline and candidate addon paths. Both use the same
JavaScript wrapper and native library. Build the baseline native addon from
the baseline revision recorded in the results and the candidate from the commit
titled `Transfer owned Lite3 buffers directly into Node`. Copy [Cargo.lock](Cargo.lock) into both native manifests' directories.
Use `cargo build --release --locked` for each with `PLASMITE_LIB_DIR` pointing at
one common optimized library. Copy each addon dylib to a distinct `.node` file.
The [environment record](release-environment.json) records their hashes and toolchain.
Dynamic-loader output confirmed the common library path during the experiment.

```sh
DYLD_LIBRARY_PATH=/path/to/common/library node bench.js /path/to/baseline.node /path/to/current.node
DYLD_LIBRARY_PATH=/path/to/common/library PLASMITE_BENCH_TEXT_BYTES=2048 \
  PLASMITE_BENCH_ITERATIONS=5000 node bench.js /path/to/baseline.node /path/to/current.node
```

The fixture uses 32 objects with numeric fields and a repeated string of 32 or
2,048 bytes. Set `PLASMITE_BENCH_ROOT` if the script sits outside its recorded
repository path. Linux uses its normal dynamic-library search configuration;
these measurements establish only macOS behavior. Electron's copy fallback
needs a host-specific run. Normal Node tests cover retained slices, independent
snapshots, pool close, forced collection, async cancellation and stream teardown.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
