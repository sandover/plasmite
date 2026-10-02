# Plasmite 1.0.0 access on Windows

Direct HTTPS Model Context Protocol (MCP) lets a client use shared pools without installing Plasmite. The host runs Plasmite; the client supplies MCP and trusts the host's certificate. On this Windows VM, direct MCP added **0.44–0.50 ms per call** over native HTTPS with an in-memory access key. Its median latency reached **2.1–2.5 times** that native client's latency, while it delivered **39–48%** of its throughput. Direct MCP ran faster than the default saved-connection native client and the remote MCP process.

## Environment and method

The measurements use committed source `4fe156a126175ff73732414f68a22f851af53001`. Its production Rust, C, binding, dependency, and vendored sources match the `v1.0.0` release tag (`865532f68d3f29d736113eaf1dbaad239d0a289c`). The archive build reports `1.0.0-dev+unknown` because the source archive omits Git metadata; this suffix does not identify different product code. Each comparison session records the archive, binary, library, addon, and harness hashes. The no-install proof records its script and server hashes.

The Windows 11 VM runs build 26200 on Arm with one virtual CPU and 7 GiB of memory. Plasmite and the native clients use the supported x64 target under Windows emulation. The client uses CPython 3.14.7, Node 24.18.0 x64, and Rust 1.88.0. The main comparison keeps the server and all clients inside Windows and uses loopback addresses. These measurements describe this VM, including x64 emulation.

Local libraries open the pool file directly. Local MCP adds a persistent process and JSON messages over its input and output pipes. Local HTTP calls the server on loopback. Remote paths use HTTPS with an access key, a browser session, or MCP authorization. Installation channels such as npm and PyPI provide these same interfaces; they do not form separate request paths. The [distribution policy](../record/distribution.md) supports Go native bindings on macOS and Linux, so this Windows comparison does not include Go.

The native Rust client offers two credential choices. Its default saved connection reloads the saved credential store before every request; Windows decrypts that store through its data protection API. `RemoteClient::with_access_key` keeps a supplied key in memory. Direct MCP also keeps its authorization token in memory. Both native HTTPS choices verify the server's public-key fingerprint through the access key. The local HTTP Rust row uses the default client, which also checks the saved store even though that loopback address needs no key.

Every request comparison uses one append or one exact fetch per call, concurrency one, fast durability, and empty tags. Each round appends 100 messages and reads every returned sequence back. Each compact JSON object occupies exactly 512 or 4,096 bytes. Its marker identifies the round, message, and client; padding keeps the byte count equal. The byte count excludes HTTP and MCP envelopes. The 256 MiB pools retain every message. Each client warms its connection with an append and read at both sizes before timing. The benchmark rotates client order across rounds.

The library and MCP processes stay alive throughout a run. Single-message CLI rows start a new process for every call. Native and Node workers time their API calls inside their persistent process; their control pipes sit outside that timer. Raw phase throughput includes the worker's encoding and verification work, while binary Lite3 call latency excludes JSON conversion. The C row includes the C ABI's JSON serialization. Python and HTTP rows include their normal JSON handling. These figures measure usable interfaces, rather than server time alone.

MCP clients perform the initialize handshake. Direct MCP completes dynamic client registration and authorization with Proof Key for Code Exchange (PKCE) against the exact `/mcp` resource, then revokes its grant after the run. The benchmark uses Python's standard HTTP client and no model provider. Authorization sits outside the timed phases. The browser row measures cookie-authenticated API requests through that HTTP client; it excludes browser scripting and page rendering. The runs do not measure concurrent clients, cold authorization, durable flushes, or Internet latency.

## All Windows request paths

Each cell shows the median of ten per-run median call latencies, in milliseconds. Two independent sessions supply five repeats each at both payload sizes. The focused credential sessions supply the Rust HTTP, native HTTPS, Python bearer HTTPS, and remote MCP rows. The broad sessions supply the remaining rows. The next table compares the principal remote paths within the same focused sessions.

| Access path | 512-byte append | 512-byte read | 4,096-byte append | 4,096-byte read |
|---|---:|---:|---:|---:|
| Local Rust, JSON | 0.042 | 0.007 | 0.048 | 0.009 |
| Local Rust, binary Lite3 | 0.039 | 0.004 | 0.042 | 0.005 |
| Local C | 0.042 | 0.007 | 0.056 | 0.014 |
| Local Python | 0.064 | 0.019 | 0.080 | 0.025 |
| Local Node | 0.072 | 0.021 | 0.086 | 0.024 |
| Local CLI, one process per call | 26.587 | 28.997 | 26.026 | 26.623 |
| Local MCP process, pool file | 0.257 | 0.211 | 0.300 | 0.219 |
| Local HTTP, Python | 0.465 | 0.375 | 0.481 | 0.398 |
| Local HTTP, Rust default client | 1.452 | 1.280 | 1.493 | 1.306 |
| Local HTTP, Node | 13.783 | 14.746 | 15.505 | 15.115 |
| Local HTTP CLI, one process per call | 34.941 | 36.868 | 34.550 | 34.727 |
| HTTPS Rust, in-memory key | 0.384 | 0.305 | 0.440 | 0.344 |
| HTTPS binary Lite3, in-memory key | 0.398 | 0.311 | 0.412 | 0.319 |
| HTTPS Rust, saved connection | 1.733 | 1.383 | 1.755 | 1.523 |
| HTTPS binary Lite3, saved connection | 1.689 | 1.564 | 1.696 | 1.568 |
| HTTPS JSON, Python bearer token | 0.504 | 0.416 | 0.532 | 0.433 |
| HTTPS browser-session API, Python | 0.591 | 0.417 | 0.507 | 0.402 |
| HTTPS Node | 13.559 | 13.252 | 15.116 | 14.845 |
| HTTPS CLI, one process per call | 38.971 | 39.966 | 35.344 | 38.814 |
| Remote MCP process over HTTPS | 3.454 | 3.522 | 3.873 | 3.517 |
| Direct HTTPS MCP, no local Plasmite | 0.868 | 0.756 | 0.944 | 0.786 |

Direct HTTPS MCP costs less than one millisecond per call here. Local file access costs much less because it avoids the server and network stack. The Node remote interface and single-call CLI have larger client costs in this environment; these results do not establish a general HTTP or HTTPS floor.

## Matched native and MCP comparison

Throughput shows the median phase rate across ten repeats; the range shows the slowest and fastest repeat. Latency shows the median of the per-run median and 95th-percentile call latencies.

| Bytes | Operation | Client | Operations/s (range) | Median / p95, ms |
|---:|---|---|---:|---:|
| 512 | Append | HTTPS Rust, in-memory key | 2,416 (1,823–2,791) | 0.384 / 0.595 |
| 512 | Append | HTTPS Rust, saved connection | 558 (427–700) | 1.733 / 2.247 |
| 512 | Append | Remote MCP process over HTTPS | 283 (229–321) | 3.454 / 4.319 |
| 512 | Append | Direct HTTPS MCP, no local Plasmite | 1,076 (903–1,374) | 0.868 / 1.268 |
| 512 | Read | HTTPS Rust, in-memory key | 3,196 (2,173–3,368) | 0.305 / 0.369 |
| 512 | Read | HTTPS Rust, saved connection | 689 (577–748) | 1.383 / 1.813 |
| 512 | Read | Remote MCP process over HTTPS | 275 (241–340) | 3.522 / 4.539 |
| 512 | Read | Direct HTTPS MCP, no local Plasmite | 1,233 (1,107–1,498) | 0.756 / 1.122 |
| 4,096 | Append | HTTPS Rust, in-memory key | 2,048 (1,154–2,635) | 0.440 / 0.725 |
| 4,096 | Append | HTTPS Rust, saved connection | 540 (399–696) | 1.755 / 2.589 |
| 4,096 | Append | Remote MCP process over HTTPS | 250 (193–323) | 3.873 / 4.799 |
| 4,096 | Append | Direct HTTPS MCP, no local Plasmite | 987 (778–1,364) | 0.944 / 1.475 |
| 4,096 | Read | HTTPS Rust, in-memory key | 2,574 (2,128–3,035) | 0.344 / 0.542 |
| 4,096 | Read | HTTPS Rust, saved connection | 604 (358–717) | 1.523 / 2.290 |
| 4,096 | Read | Remote MCP process over HTTPS | 277 (176–327) | 3.517 / 4.386 |
| 4,096 | Read | Direct HTTPS MCP, no local Plasmite | 1,200 (546–1,400) | 0.786 / 1.138 |

Keeping the native key in memory removes most of the saved-connection cost on this Windows VM. The saved client deliberately reloads credentials so changes take effect between requests. Its comparison with direct MCP therefore includes a credential-policy difference. The in-memory native row gives the closer comparison with MCP's persistent authorization token.

## CLI batches

Each batch writes 100 JSONL messages through one `feed` process, then reads the retained batch through one `follow --tail 100 --no-follow --json` process. Every history read verifies sequence and content. These rates include process startup and pipe input/output. The first batch has no separate warm-up. History streaming performs different work from 100 separate exact fetches.

| Batch path | 512-byte writes/s | 512-byte history reads/s | 4,096-byte writes/s | 4,096-byte history reads/s |
|---|---:|---:|---:|---:|
| Local CLI | 3,294 | 3,175 | 2,912 | 2,200 |
| Local HTTP CLI | 616 | 2,358 | 612 | 1,869 |
| HTTPS CLI | 543 | 2,217 | 532 | 1,777 |

Local batch writes reach roughly 3,000 messages per second. This explains why the cost of starting one CLI process per message does not describe its sustained feed rate.

## Windows client across the VM network

The Windows client also calls a separate Mac server through VMware's private host/guest network. The server runs the installed Plasmite 1.0.0 binary on macOS 26.6.2, Apple M5 Pro, arm64, with 48 GiB of memory. Its binary SHA-256 is `4d23d2f67d2dac78cc05ac9f5771ad212d7909ebb3e25aa069c926a6b086df30`. This test adds a host/guest network hop and changes server hardware; it does not isolate network overhead. The table shows median call latency in milliseconds across five repeats per payload size.

| Windows client → Mac server | 512-byte append | 512-byte read | 4,096-byte append | 4,096-byte read |
|---|---:|---:|---:|---:|
| HTTPS Rust, saved connection | 1.591 | 1.692 | 1.848 | 1.853 |
| HTTPS Rust, in-memory key | 0.245 | 0.247 | 0.336 | 0.308 |
| HTTPS binary Lite3, saved connection | 1.559 | 2.304 | 1.898 | 1.815 |
| HTTPS binary Lite3, in-memory key | 0.265 | 0.229 | 0.348 | 0.294 |
| HTTPS JSON, Python bearer token | 0.374 | 0.305 | 0.537 | 0.407 |
| HTTPS browser-session API, Python | 0.363 | 0.311 | 0.473 | 0.368 |
| HTTPS Node | 14.736 | 14.011 | 15.403 | 15.020 |
| HTTPS CLI, one process per call | 93.675 | 94.509 | 93.526 | 95.203 |
| Remote MCP process over HTTPS | 3.471 | 3.458 | 3.748 | 3.954 |
| Direct HTTPS MCP, no local Plasmite | 0.459 | 0.502 | 0.604 | 0.539 |

A separate direct-MCP check rejects every client subprocess and every import of the native `plasmite` module through a Python audit hook. It completes authorization, initialization, 1,000 appends, and 1,000 verified exact reads against that Mac server. This checks the no-install path at the protocol boundary. The raw proof includes the validation script.

## Reproduce

Use a dedicated pool and disposable access key. Keep the key file owner-only. The request runner creates a private saved-connection store and removes it after clients stop, so it leaves existing saved connections alone. It accepts a JSON key file with an `access_key` field or a plain-text key. Use a CA file that trusts the server's certificate. See [serving](../record/serving.md) for server and trust setup and [building](../building.md) for the Windows compiler prerequisites.

From an x64 Visual Studio developer terminal with `clang-cl` on `PATH`, build the CLI, library, and native helpers:

```powershell
cargo +1.88.0-x86_64-pc-windows-msvc build --release --bin plasmite --lib --example bench_transport_native
clang-cl /c /O2 /W4 /WX /Iinclude scripts\bench_access_c.c /Fobench_access_c.obj
link.exe /OUT:target\release\bench_access_c.exe /LIBPATH:target\release bench_access_c.obj plasmite.dll.lib
cargo +1.88.0-x86_64-pc-windows-msvc build --release --manifest-path bindings\node\native\Cargo.toml
New-Item -ItemType Directory -Force bindings\node\native\win32-x64 | Out-Null
Copy-Item bindings\node\native\target\release\plasmite_node.dll bindings\node\native\win32-x64\index.node
Copy-Item target\release\plasmite.dll bindings\node\native\win32-x64\plasmite.dll
$env:PLASMITE_LIB_DIR = "$PWD\target\release"
$env:PATH = "$PWD\target\release;$env:PATH"
```

Use x64 Node 24 or newer and Python 3.10 or newer. The Python runners need only the standard library. Create the pool, run a local HTTP and secure HTTPS listener for it, and invite a disposable key. For example, `plasmite --dir $PoolDir pool create access-bench --size 256M` creates the test pool. Grant the Windows account sole access to its key file with `icacls $KeyFile /inheritance:r /grant:r "${env:USERNAME}:(F)"`.

Run the request comparison twice, changing the output filename for the second session:

```powershell
python scripts\bench_access_comparison.py `
  --pool-dir $PoolDir --pool access-bench `
  --server $SecureServer --local-server $LocalServer `
  --access-key-file $KeyFile --ca-file $CaFile `
  --node $NodeX64 --output access-session1.raw.json
```

The defaults cover all 21 request paths with 100 messages, five repeats, and both sizes. Select individual paths with `--lanes`; for example, `--lanes https_rust_key https_rust remote_stdio_mcp direct_https_mcp` reproduces the principal remote comparison. For direct MCP alone, select `--lanes direct_https_mcp`; that path needs no local CLI, library, or Node build, and `--root` may point at a directory without Plasmite binaries. The common argument parser still accepts the unused pool-directory and local-server values.

The batch runner uses the current saved-connection store. Point `PLASMITE_ACCESS_HOME` at a disposable directory, run `plasmite access connect $SecureServer` with the test key, and then run:

```powershell
python scripts\bench_access_streaming.py `
  --binary target\release\plasmite.exe `
  --pool-dir $PoolDir --pool access-bench `
  --server $SecureServer --local-server $LocalServer `
  --output access-stream-session1.raw.json
```

Stop the dedicated server and remove its disposable pool, key, and connection directory after the run. The runners verify every read and stop on a mismatch. Record source revision, artifact hashes, client versions, and machine configuration alongside future results.

## Evidence

The timed runs complete 138,000 verified operations: 72,000 in the broad comparison, 32,000 in the focused credential comparison, 12,000 in CLI batches, 20,000 across the VM network, and 2,000 in the no-install check. The final short Windows check also exercises all 21 request paths at both sizes after the runner's last changes, with 504 verified operations. `just check` passes, including formatting, lint, Rust tests, version alignment, release-target checks, and Lite3 integrity. The server-restart test failed once, then passed its focused retry and the complete check. Python compilation, Node syntax checks, C compiler warnings, and peer review also pass.

- Broad comparison: [session 1](windows-access-1.0-session1.raw.json), [session 2](windows-access-1.0-session2.raw.json)
- Focused credentials: [session 1](windows-access-1.0-keys-session1.raw.json), [session 2](windows-access-1.0-keys-session2.raw.json)
- CLI batches: [session 1](windows-access-1.0-stream-session1.raw.json), [session 2](windows-access-1.0-stream-session2.raw.json)
- Separate server: [VM network](windows-access-1.0-network.raw.json), [no-install proof](windows-access-1.0-noinstall.raw.json)
- Final runner: [all-path smoke check](windows-access-1.0-smoke.raw.json)
