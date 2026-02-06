<!--
Purpose: Baseline scan-only `get(seq)` latency before inline index format changes.
Exports: N/A (benchmark record for later before/after comparison).
Role: Reference artifact for task 3SS7GN and downstream perf analysis.
Invariants: Parameters remain fixed across runs; values are medians of three release runs.
-->

# get_scan baseline (pre-index)

Captured on 2026-02-06 using `target/release/examples/plasmite-bench`.

## Run configuration

- Build profile: `release`
- OS/arch: `macos` / `aarch64`
- CPUs: `8`
- Durability: `fast`
- Pool sizes: `1 MiB` and `8 MiB`
- Payload size: `1024` bytes
- Message count loaded before each `get(seq)`: `20000`
- Writers: `1`
- Runs per case: `3` (median reported)

Command:

```bash
./target/release/examples/plasmite-bench \
  --format json \
  --pool-size 1M --pool-size 8M \
  --payload-bytes 1024 \
  --writers 1 \
  --messages 20000 \
  --durability fast
```

## Results (ms per get, median of 3 runs)

| pool_size_bytes | pool_size | position      | samples_ms                         | median_ms_per_get |
|---:|---:|---|---|---:|
| 1048576 | 1 MiB | near_newest | 0.019125, 0.020500, 0.021042 | 0.020500 |
| 1048576 | 1 MiB | mid         | 0.008916, 0.009542, 0.009542 | 0.009542 |
| 1048576 | 1 MiB | near_oldest | 0.000042, 0.000083, 0.000084 | 0.000083 |
| 8388608 | 8 MiB | near_newest | 0.150708, 0.153125, 0.159667 | 0.153125 |
| 8388608 | 8 MiB | mid         | 0.073167, 0.073416, 0.079000 | 0.073416 |
| 8388608 | 8 MiB | near_oldest | 0.000041, 0.000083, 0.000250 | 0.000083 |

## Notes

- Raw benchmark outputs are in `tmp/bench-baseline/run1.json`, `tmp/bench-baseline/run2.json`, and `tmp/bench-baseline/run3.json`.
- This file is the pre-index baseline required before any index-region format changes.

---

# post-index comparison (task XS2YXU)

Captured on 2026-02-06 using the same command and parameters as the baseline (three release runs; median reported):

```bash
./target/release/examples/plasmite-bench \
  --format json \
  --pool-size 1M --pool-size 8M \
  --payload-bytes 1024 \
  --writers 1 \
  --messages 20000 \
  --durability fast
```

`plasmite-bench` now records both:
- `indexed:*` (`get(seq)` with default auto index)
- `scan_only:*` (`get(seq)` on new format with `index_capacity=0`)

## Before vs after `get(seq)` latency (ms, median of 3 runs)

| pool_size | position | baseline_old_scan_ms | indexed_new_ms | scan_only_new_ms | indexed_speedup_vs_old |
|---|---|---:|---:|---:|---:|
| 1 MiB | near_newest | 0.020500 | 0.000375 | 0.027125 | 54.67x |
| 1 MiB | mid | 0.009542 | 0.000125 | 0.008833 | 76.34x |
| 1 MiB | near_oldest | 0.000083 | 0.000167 | 0.000125 | 0.50x |
| 8 MiB | near_newest | 0.153125 | 0.000250 | 0.806167 | 612.50x |
| 8 MiB | mid | 0.073416 | 0.000792 | 0.073792 | 92.70x |
| 8 MiB | near_oldest | 0.000083 | 0.000375 | 0.000042 | 0.22x |

## Append regression check (`append`, core payload reused)

| pool_size | baseline_old_ms_per_msg | indexed_new_ms_per_msg | regression |
|---|---:|---:|---:|
| 1 MiB | 0.022580 | 0.023270 | +3.05% |
| 8 MiB | 0.022489 | 0.023307 | +3.64% |

## Interpretation

- Indexed gets are dramatically faster for `mid` and `near_newest` lookups.
- For `near_oldest`, indexed lookup is not the fast path because index slots are overwritten by newer entries.
- Scan-only-on-new-format is roughly neutral for `mid`, but `near_newest` regressed (especially at 8 MiB); this indicates a scan-path behavior change worth follow-up.
- Append overhead from index slot writes is under the <5% target in both pool sizes.

## Raw artifacts

- Pre-index: `tmp/bench-baseline/run1.json`, `tmp/bench-baseline/run2.json`, `tmp/bench-baseline/run3.json`
- Post-index: `tmp/bench-post-index/run1.json`, `tmp/bench-post-index/run2.json`, `tmp/bench-post-index/run3.json`
