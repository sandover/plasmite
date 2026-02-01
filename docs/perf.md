# Performance baselines

Plasmite is intentionally simple and local-first, but we still want to know what things cost.
This repo includes a small benchmark suite to establish a baseline and to quantify improvements
over time.

## Run

Default suite (JSON to stdout, table to stderr):

```bash
cargo build --release --example plasmite-bench
./target/release/examples/plasmite-bench
```

JSON only (easy to archive/compare):

```bash
./target/release/examples/plasmite-bench --format json > bench.json
```

Customize the parameter grid:

```bash
./target/release/examples/plasmite-bench \
  --pool-size 1M --pool-size 64M \
  --payload-bytes 128 --payload-bytes 1024 --payload-bytes 16384 \
  --writers 1 --writers 2 --writers 4 --writers 8 \
  --messages 20000
```

Compare durability modes (flush vs best-effort):

```bash
./target/release/examples/plasmite-bench --durability fast --durability flush
```

Use a specific work directory (keeps pool files/artifacts around):

```bash
./target/release/examples/plasmite-bench --work-dir .scratch/bench
```

## What it measures (current)

- `append`: append throughput (two variants)
  - “core payload reused”
  - “includes Lite3 encode per msg”
- `follow`: follow-style read throughput/latency (spawns a writer + follower process)
- `get_scan`: scan-based `get` cost for `seq` near newest / middle / oldest
- `multi_writer`: contention overhead with multiple writer processes appending concurrently
- Each result includes a `durability` field to distinguish `fast` vs `flush`.
- The table groups by pool/payload and includes an `x_fast` ratio when a fast baseline exists.
- Flush benchmarks are sampled by default (one representative pool/payload and a single multi-writer count).

## Caveats

- Benchmarks are designed for **trend tracking**, not absolute cross-machine comparisons.
- Results can vary with CPU frequency scaling, filesystem, system load, and build profile.
- Some scenarios spawn child processes to better approximate real-world cross-process locking.
