# Live watcher capacity

On one Apple silicon Mac, the 2.0 baseline added a waiting thread for each Model
Context Protocol (MCP) Events stream. At 64 streams, it delivered every matching
notification, but finite HTTP fetches returned HTTP 423 until a stream closed.

## Release baseline on macOS

The Apple M5 Pro ran macOS 27.2 (arm64). Each row shows one local loopback
HTTP/1.1 trial with release build `234cc5b`.

| Readers | Threads before → idle | Resident memory before → idle | Idle CPU | Tagged receipts | p95 to last receipt |
| --- | ---: | ---: | ---: | ---: | ---: |
| [1 MCP stream](results/baseline-macos-event-1.json) | 17 → 17 | 9.80 → 9.89 MiB | 0.03% | 50/50 | 2.34 ms |
| [64 MCP streams](results/baseline-macos-event-64.json) | 17 → 80 | 9.72 → 16.45 MiB | 1.15% | 3,200/3,200 | 5.30 ms |
| [1 HTTP follower](results/baseline-macos-http-1.json) | 17 → 17 | 8.14 → 8.22 MiB | 0.04% | 50/50 | 9.68 ms |

At 64 streams, all ten feeds and MCP info calls succeeded. All ten finite
fetches received HTTP 423. The warm-up used one blocking worker, so the first
reader could reuse it. These figures describe individual runs. Rerun both
builds to compare changes against run-to-run variation.

## Repeat the baseline

Build the recorded revision in a separate checkout. Run the benchmark from the
current checkout with [uv](https://docs.astral.sh/uv/). Each local trial starts a
release binary on loopback with its own pool directory and stops it afterward.

```sh
scratch=$(mktemp -d)
git worktree add --detach "$scratch/baseline" 234cc5b
cargo build --release --manifest-path "$scratch/baseline/Cargo.toml" --bin plasmite
baseline="$scratch/baseline/target/release/plasmite"

for streams in 1 64; do
  uv run docs/performance/watcher-capacity/measure.py \
    --binary "$baseline" --revision 234cc5b --events "$streams" \
    --output "$scratch/baseline-$streams.json"
done
uv run docs/performance/watcher-capacity/measure.py \
  --binary "$baseline" --revision 234cc5b --events 0 --http-follows 1 \
  --output "$scratch/baseline-http-1.json"
git worktree remove "$scratch/baseline"
```

The default publishes 100 messages at 20 per second. Each message contains 1 KiB
of text. Every other message carries the tag readers request. The clock starts
just before the producer sends an HTTP feed and stops when the last matching
reader receives its event. The benchmark checks every expected receipt and
counts duplicate, out-of-order and nonmatching receipts as errors. Idle CPU
measures five seconds of server process time as a fraction of one core. Resident
memory includes mapped pool pages. Before loading the server, the benchmark
warms one connection and measures how long feeds, finite fetches and MCP pool
info calls take.

## Measure HTTPS and mixed load

The benchmark can launch its own HTTPS listener on loopback. It generates a
certificate and a disposable access key, then creates a synthetic MCP grant in
its temporary directory. Plasmite checks both credentials on its normal
HTTPS routes. This measures the connection and read paths, including
authentication on each request. The benchmark deletes the temporary server
directory after the trial.

```sh
uv run docs/performance/watcher-capacity/measure.py \
  --binary "$candidate" --https --revision "$revision" \
  --connections shared-h2 --events 1000 --http-follows 16 --mcp-follows 16 \
  --pools 10 --stall --rates 20 50 100 200 --seconds 15 --controls 30 \
  --idle-settle-seconds 12 --idle-seconds 35 \
  --output "$scratch/shared-http2.json"
```

You can also point the benchmark at a dedicated test listener with `--url`.
Use `--help` to see the certificate, credential file, and process ID options.

Repeat with `--connections separate-h1` to give each watcher its own HTTP/1.1
connection. In the HTTP/2 run, groups of up to 50 watchers reuse a connection,
and the first group makes short requests on that connection. In the HTTP/1.1
run, the benchmark opens a new connection for an ordinary call after the
watchers open. Use a client and server file descriptor limit high enough for
separate connections, and record the limit alongside the results.

Run the fixed-rate check with 1, 64, 256 and 1,000 Events streams. Repeat each
count across one and ten pools, where the reader count permits. For the mixed
run, increase the target write rate and compare it with the rate the producer
achieved. Check watcher admission, delivery counts and short-request p95 at each
rate. Compare the before-and-after builds at the highest common reader count if
the baseline refuses the target load.

With `--stall`, a separate HTTP follower stops reading. The producer writes
24 MiB to that pool to fill the network buffers and the server's output queue.
The benchmark records server memory before opening readers, after they go idle,
and after filling the stalled follower's queue. It times short requests again
while still sending to the other readers. This test estimates server memory
under the workload, including the pages the stall writes touch.
`--idle-settle-seconds` gives unused blocking workers time to exit before the
idle sample. A 35-second sample also includes the normal 30-second stream
heartbeat. Use the same
payload sizes and rates for each build. Alternate before-and-after runs and
compare the spread before drawing conclusions.
