# Python asyncio benchmarks

## Native wait probe

On macOS 27.2.1 (ARM64) with Python 3.14.10, the probe used the local debug library from the working tree at `94dd55a`. One thread blocked on the same native pool notification the server uses. The probe used the server’s one-second recovery interval, checking the pool once after a timeout. It received all 100 writes per reader through native signals, with no deliveries handled by recovery. Idle CPU measures process time over five quiet seconds as a share of one core. Receipt p95 runs from the start of a feed to the last reader receiving its sequence.

| Followers | Handoff | Added wait threads | Idle CPU | p95 to last receipt |
| ---: | --- | ---: | ---: | ---: |
| 1 | Callback | 1 | 0.029% | 0.206 ms |
| 1 | Socket | 1 | 0.028% | 0.363 ms |
| 100 | Callback | 1 | 0.024% | 1.327 ms |
| 100 | Socket | 1 | 0.024% | 1.909 ms |

This probe measures the native wait and Python handoff on one local pool. The full binding benchmark below includes the event loop, read workers, message delivery, and callback for each waiting reader.

To repeat the macOS probe from the repository root, build the library and stage the matching Python library if needed.

```sh
cargo build --lib
bash scripts/stage_versioned_library.sh target/debug
probe_dir=$(mktemp -d)
rustc --edition 2024 --crate-type cdylib \
  --extern plasmite=target/debug/libplasmite.rlib -L dependency=target/debug/deps \
  bindings/python/benchmarks/async_wake_native.rs -o "$probe_dir/async_wake.dylib"
PLASMITE_LIB_DIR="$PWD/target/debug" PYTHONPATH="$PWD/bindings/python" \
  uv run --no-project python bindings/python/benchmarks/async_wake.py \
  --native "$probe_dir/async_wake.dylib"
```

## Asyncio runtime measurements

On the same Apple Silicon Mac with CPython 3.14.10 and a Rust release build, the full binding delivered 100 writes to local readers after five quiet seconds. Each sample measured time from the first feed to the last reader receiving its message. Loop delay measures how far a 1 ms asyncio timer ran past its deadline. In the last row, a second writer sent untagged messages to the same pool about every 200 µs during the paced test.

| Readers | Pools | Second writer | Quiet CPU, % of one core | p95 last receipt, ms | p95 loop delay, ms |
| ---: | ---: | :--- | ---: | ---: | ---: |
| 1 | 1 | No | 0.013 | 0.457 | 0.144 |
| 100 | 1 | No | 0.011 | 4.194 | 0.638 |
| 100 | 100 | No | 0.424 | 6.265 | 2.814 |
| 100 | 1 | Yes | 0.013 | 5.280 | 0.713 |

Once Python's bounded read workers had started, the 100-pool run added 100 threads and the final one-pool run added one. A separate reader on the same pool reuses its native waiter. These measurements cover macOS. The asyncio integration tests also exercise real authenticated HTTPS and UDP listeners on the same host.

```sh
cargo build --release --lib
bash scripts/stage_versioned_library.sh target/release
PLASMITE_LIB_DIR="$PWD/target/release" PYTHONPATH="$PWD/bindings/python" \
  uv run --no-project python bindings/python/benchmarks/async_runtime.py
```
