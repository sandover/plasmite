# Live watcher capacity

The shared-wait implementation served 1,000 Model Context Protocol (MCP) Events
streams, 16 HTTP followers and 16 MCP follow calls across ten pools. The release
build delivered every tagged message to every matching reader in the mixed
trials below. The longer HTTP/1.1 repeats met the short-request target on
macOS. The HTTP/2 write-rate trials found cases where short requests exceeded
twice their idle latency.

## HTTPS mixed load

Each row uses 20 writes per second, 1 KiB of text per message, and one HTTP
follower that stopped reading while the other readers received their messages.
Idle memory and thread samples come from before the extra follower stalls.
The macOS runs used release build `687025b`. The Debian 13 ARM64 virtual machine
had four virtual CPUs and ran release build `1ed2128`, which includes the
non-Apple build fix. The Windows 11 ARM virtual machine ran the x64 release
executable from `1ed2128` under emulation. All trials ran on the Apple M5 Pro.

| Host and connection shape | Server idle threads | Idle CPU | Idle resident memory | p95 to last receipt |
| --- | ---: | ---: | ---: | ---: |
| [macOS, shared HTTP/2](results/macos-h2-mixed.json) | 29 | 0.09% | 19.67 MiB | 12.73 ms |
| [macOS, separate HTTP/1.1, two repeats](results/macos-h1-mixed-settled.json) | 49–57 | 0.16–0.20% | 55.50–55.91 MiB | 13.46–15.52 ms |
| [Debian ARM64 VM, shared HTTP/2](results/debian-arm64-h2-mixed.json) | 29 | 0.20% | 19.75 MiB | 47.35 ms |
| [Debian ARM64 VM, separate HTTP/1.1](results/debian-arm64-h1-mixed.json) | 35 | 0.20% | 43.12 MiB | 16.60 ms |
| [Windows x64 emulated, shared HTTP/2](results/windows-x64-emulated-h2.json) | 27 | 0.00% | 44.52 MiB | 34.78 ms |
| [Windows x64 emulated, separate HTTP/1.1](results/windows-x64-emulated-h1.json) | 31 | 0.00% | 71.10 MiB | 35.21 ms |

The five-second trials delivered 5,160 tagged notifications or messages each.
The macOS HTTP/1.1 repeats lasted ten seconds and delivered 10,320 each.
All ordinary calls succeeded with the streams open. With the stalled reader,
feed, finite fetch, and MCP info p95 stayed below twice their idle times in
the macOS and Debian rows. Finite fetch p95 exceeded twice idle in both
Windows emulation trials. MCP info p95 also exceeded twice idle on Windows
shared HTTP/2. Native Linux and Windows x64 capacity checks remain open.

In [the first short macOS HTTP/1.1 trial](results/macos-h1-mixed-quick.json),
MCP info p95 rose from 8.29 to 31.37 ms. The two repeats used 40 samples per
request instead of 12 and waited 12 seconds after opening the readers.
The server still had 49–57 threads after the wait, with ten pool waiters
and MCP follow calls that renewed every five seconds.

On macOS, the separate-connection trial used 1,054 server descriptors with
the readers idle, compared with 43 for shared HTTP/2. In the five-second
trials with a stalled follower, resident memory reached 68.38 MiB on HTTP/1.1
and 28.59 MiB on HTTP/2. These samples include the pool pages the benchmark
touched.

The [Debian baseline](results/baseline-debian-arm64-event-64.json) admitted 64
quiet local streams and grew from six to 69 idle threads. It used 1.00% of one
core and returned HTTP 423 for every finite fetch attempted while the streams
were open. Its [single-stream baseline](results/baseline-debian-arm64-event-1.json)
is available for comparison.

## Additional event streams on macOS

The [28-trial count comparison](results/macos-scaling.json) opened 1, 64,
256 and 1,000 Events streams with both HTTPS connection shapes. It repeated
every multi-stream count across one and ten pools and alternated the two
release builds. Each trial sent 40 writes at 20 per second and sampled idle
CPU for one second.

The baseline admitted at most 64 streams and held 80 threads at that
count. The candidate admitted every requested stream. With 1,000 streams
watching one pool, it held 20 threads on shared HTTP/2 and 33 threads on
separate HTTP/1.1. With the streams spread across ten pools, those counts
were 29 and 51. Every admitted reader received each matching message.
The candidate also accepted an ordinary request with the streams open.

A [settled HTTP/1.1 trial](results/macos-h1-events-settled.json) with 1,000
Events streams across ten pools had 17 threads before readers, 55 at
admission and 26 after a 15-second wait and a five-second idle sample.
The server used 0.033% of one core during the sample and delivered all
2,000 matching notifications.

## Raise the write rate

Each step ran for eight seconds with 1,032 readers and one stalled follower.
Both macOS connection shapes and Debian ARM64 shared HTTP/2 committed all
scheduled writes and delivered all matching messages. The highest measured
rate was 100 writes and 5,160 matching receipts per second.

| Writes per second | Tagged receipts per trial | macOS, shared HTTP/2 p95 | macOS, separate HTTP/1.1 p95 | Debian ARM64, shared HTTP/2 p95 |
| ---: | ---: | ---: | ---: | ---: |
| 20 | 8,256/8,256 | 11.80 ms | 14.17 ms | 48.27 ms |
| 50 | 20,640/20,640 | 10.91 ms | 13.09 ms | 51.17 ms |
| 100 | 41,280/41,280 | 9.14 ms | 11.53 ms | 49.44 ms |

All short requests succeeded. The latency target remains open. At 100
writes per second, finite fetch p95 reached 2.37 times idle on macOS shared
HTTP/2, 3.29 times idle on macOS separate HTTP/1.1 and 14.58 times idle on
Debian shared HTTP/2. On Debian the loaded p95 was about 42–46 ms in the
two faster trials.

See the [macOS HTTP/2](results/macos-h2-ramp.json),
[macOS HTTP/1.1](results/macos-h1-ramp.json), and
[Debian ARM64 HTTP/2](results/debian-arm64-h2-ramp.json) samples.

### TLS socket delay on Debian

At 50 writes per second for five seconds, [the control build](results/debian-arm64-delay-control.json)
and [the modified build](results/debian-arm64-delay-nodelay.json) each delivered
13,416 matching receipts. The modified build enabled `TCP_NODELAY` on accepted
sockets before the TLS handshake. The p95 to the last receipt fell from
50.90 ms to 9.20 ms. Plasmite now enables the option on HTTPS sockets.
The short fetch p95 under load was 2.57 ms in the control
and 3.62 ms with the option. This pair leaves the cause of the intermittent
short-request delays open.

## One reader on macOS

[Three alternating pairs](results/macos-single-pairs.json) sent two seconds
of local loopback traffic at 20 writes per second. Every trial delivered all
20 tagged messages.

| Reader | Baseline p95 range | Candidate p95 range |
| --- | ---: | ---: |
| One MCP Events stream | 2.30–3.24 ms | 3.24–3.67 ms |
| One HTTP follower | 2.24–3.14 ms | 3.53–3.70 ms |

The Debian and Windows virtual machines were also working during those
pairs. [A quiet repeat](results/macos-single-quiet.json) used five seconds
per trial, with both virtual machines off and the build and count trials
complete. It delivered all 50 tagged messages in every run. Events p95
went from 3.12 to 3.42 ms, and HTTP follow p95 went from 2.96 to 3.64 ms.
The single-reader latency target remains open. The candidate schedules a
short read after the common waiter signals. In the baseline, the same
blocking thread waits and reads.

[A separate local socket test](results/macos-local-nodelay.json) set
`TCP_NODELAY` on the loopback listener. Events p95 went from 3.74 to
3.76 ms, and HTTP follow p95 went from 3.54 to 3.49 ms. The setting
did not recover the single-reader latency. The production option applies
to HTTPS sockets.

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
    --binary "$baseline" --legacy-pool-routes --revision 234cc5b --events "$streams" \
    --output "$scratch/baseline-$streams.json"
done
uv run docs/performance/watcher-capacity/measure.py \
  --binary "$baseline" --legacy-pool-routes --revision 234cc5b --events 0 --http-follows 1 \
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
