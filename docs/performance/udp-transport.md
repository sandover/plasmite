# UDP and HTTP feed/follow on loopback

On this Mac, both transports delivered about 195–199 useful messages per second when one sender offered 200 per second. That rate comes from the fixed workload, so it says nothing about either transport's maximum throughput. UDP delivered 8,997 of 9,000 expected follower copies. HTTP delivered 9,000 of 9,000. The three UDP skips occurred with 16 KiB data and four followers. Every send returned success in the final run, which shows why a successful UDP send cannot stand in for delivery or storage confirmation.

The Rust API uses separate paths. `UdpPool::send_lite3` returns when the operating system accepts a datagram. `RemotePool::append_lite3` returns a committed sequence from the HTTP server. UDP follow uses its connected UDP socket; it does not call HTTP. The table measures send start to receipt of the full, exact Lite3 attachment by a follower. It does not use the pool timestamp as a commit clock.

## Measured results

Each row covers three runs of 200 sends. A four-follower row expects 2,400 copies; a one-follower row expects 600. Rates and latency percentiles are medians of the per-follower rows in [the raw CSV](udp-transport-results/loopback.csv). Each follower's rate equals its received count divided by the interval from the first send to its last receipt. Latencies use the local monotonic clock. The table shows the 50th, 95th, and 99th percentiles of send-start-to-follower-receipt time in milliseconds.

| Data blob | Followers | Transport | Received / expected | Gaps | Useful messages/s | Receipt p50 / p95 / p99 ms |
| ---: | ---: | --- | ---: | ---: | ---: | ---: |
| 128 B | 1 | UDP | 600 / 600 | 0 | 198.7 | 27.14 / 50.27 / 54.95 |
| 128 B | 1 | HTTP | 600 / 600 | 0 | 195.8 | 29.20 / 51.77 / 55.15 |
| 128 B | 4 | UDP | 2,400 / 2,400 | 0 | 197.2 | 27.23 / 50.96 / 54.62 |
| 128 B | 4 | HTTP | 2,400 / 2,400 | 0 | 196.7 | 27.62 / 51.79 / 54.52 |
| 1 KiB | 1 | UDP | 600 / 600 | 0 | 198.0 | 26.50 / 50.76 / 54.79 |
| 1 KiB | 1 | HTTP | 600 / 600 | 0 | 196.6 | 28.42 / 52.09 / 55.27 |
| 1 KiB | 4 | UDP | 2,400 / 2,400 | 0 | 197.5 | 27.01 / 50.87 / 53.77 |
| 1 KiB | 4 | HTTP | 2,400 / 2,400 | 0 | 196.4 | 28.10 / 51.70 / 55.05 |
| 16 KiB | 1 | UDP | 600 / 600 | 0 | 196.3 | 28.19 / 51.18 / 54.29 |
| 16 KiB | 1 | HTTP | 600 / 600 | 0 | 194.5 | 29.41 / 53.13 / 55.27 |
| 16 KiB | 4 | UDP | 2,397 / 2,400 | 3 | 196.5 | 27.14 / 50.75 / 53.96 |
| 16 KiB | 4 | HTTP | 2,400 / 2,400 | 0 | 195.8 | 28.09 / 51.95 / 54.21 |

The UDP gap counts account for one missing copy in run 2 and two in run 3. No follower reported an error. All 3,600 UDP sends returned OS success; all 3,600 HTTP appends returned committed sequences. The send-return median ranged from 0.025 to 0.060 ms for UDP and 0.438 to 0.690 ms for HTTP. These return times end at different events and must not serve as a speed comparison for committed writes.

## Workload and limits

The harness prepares the same Lite3 bytes for both transports before timing. Each message contains an `id` and an ASCII `blob` of 128, 1,024, or 16,384 bytes. The full Lite3 attachments measure 561, 1,457, and 16,817 bytes. A serial sender starts one send every 5 ms, with no retries. Each run gets a fresh 8 MiB pool, and followers open before the first send. HTTP uses the trusted local loopback listener, `RemoteClient`, fast durability, and the binary `tail_lite3` stream. UDP uses `UdpClient`, `send_lite3`, and `UdpFollow`. Both paths check each received sequence and complete attachment against the prepared bytes.

This setup measures one machine's listener, storage, client, scheduling, and loopback path together. It does not measure remote HTTPS, a physical network, saturated throughput, durability after a crash, or UDP delivery under sustained overload. The 5 ms pacing limits useful throughput to about 200 messages per second. It also leaves the follower's roughly 50 ms high-percentile timing visible on both paths. A missing UDP copy can come from a socket queue, send pressure, or later delivery behavior; the gap event does not identify where it went.

The host was a `Mac17,9` with 15 logical CPUs, arm64, macOS 26.6.2 (25G83), Rust and Cargo 1.88.0. The source checkout was dirty on `main` at `528e26445f92`; the release binary used in the final run had SHA-256 `0b3f9a71a7a7b1e1faa0c6e9dcaabb058ec028cdcdb3795e0678082605de702e`. [The run log](udp-transport-results/run.log) records the ephemeral listener addresses and workload. The raw CSV has one row per follower per run, including send failures, received and missing counts, gaps, rate, and latency percentiles.

## Socket-size check

The first release build used macOS's default UDP send buffer. This host reports `net.inet.udp.maxdgram: 9216`; every 16 KiB UDP send failed with `EMSGSIZE` even though the protocol allows larger packets. [The pre-fix CSV](udp-transport-results/pre-fix.csv) preserves that discovery. The client and server now set 128 KiB UDP send and receive buffers. The final matrix above uses a rebuilt release binary with that fix.

The [maximum-packet probe](udp-transport-results/max-probe.txt) sent one valid 65,467-byte Lite3 attachment and received the exact bytes through UDP follow. Its 40-byte data header made a 65,507-byte packet, the protocol maximum. The client rejected the next, 65,468-byte attachment before sending. This checks the actual socket path as well as the size guard; it does not guarantee that every host permits the same packet size.

## Reproduce

Run from the repository root on a host that permits loopback sockets. The harness starts one local Plasmite server, creates disposable pools under the system temporary directory, then stops the server and removes that directory. It makes no changes to saved access settings.

```sh
cargo build --release --bin plasmite --lib
PLSM_SERDE_LIB=$(find target/release/deps -maxdepth 1 -name 'libserde_json-*.rlib' -print -quit)
rustc --edition=2024 -O docs/performance/udp-transport-bench.rs \
  --extern plasmite=target/release/deps/libplasmite.rlib \
  --extern serde_json="$PLSM_SERDE_LIB" \
  -L dependency=target/release/deps \
  -o /tmp/plasmite-udp-transport-bench
/tmp/plasmite-udp-transport-bench target/release/plasmite > /tmp/plasmite-udp-loopback.csv
/tmp/plasmite-udp-transport-bench target/release/plasmite --max-probe
```

The CSV contains three runs for every combination of 128 B, 1 KiB, and 16 KiB blobs, one or four followers, and UDP or HTTP. The maximum-packet probe runs separately so its single datagram does not affect the paced comparison.
