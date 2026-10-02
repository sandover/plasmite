# Native and MCP transport performance

The measurements below are historical. They were collected on 2026-09-26 on one macOS host and compare three lanes: the native API, local Model Context Protocol (MCP) over the stdio bridge, and direct Hypertext Transfer Protocol Secure (HTTPS) MCP. The stdio bridge used `plasmite mcp --remote` and has since been removed. The table and raw sessions preserve those measurements; the current harness compares only the native API and direct HTTPS MCP.

In those historical runs, the long-lived native API was fastest. Across ten runs per condition, local MCP delivered 45–51% of native exact-read and append throughput, while direct HTTPS MCP delivered 71–78%.

The measurements compare one client and server build on loopback. Each lane used the same pool, 100 individual operations per phase, sequential concurrency, and the same JSON application content. Append and exact-read timings were separate. The results do not include process startup, interactive login, or model response time.

## Historical results from 2026-09-26

Throughput is the median across ten timed runs; the range shows the slowest and fastest run. Latency shows the median of the per-run median and p95 call latency. The percentage compares each MCP lane's median throughput with the native API median for the same payload and operation.

| JSON content bytes | Operation | Lane | Throughput, ops/s (range) | Median / p95 latency, ms | Native throughput |
|---:|---|---|---:|---:|---:|
| 512 | Append | Native API | 4,759 (3,775–4,859) | 0.204 / 0.242 | 100% |
| 512 | Append | Local MCP | 2,335 (2,272–2,413) | 0.416 / 0.487 | 49% |
| 512 | Append | HTTPS MCP | 3,731 (3,336–3,783) | 0.260 / 0.311 | 78% |
| 512 | Exact read | Native API | 5,496 (5,432–5,824) | 0.177 / 0.202 | 100% |
| 512 | Exact read | Local MCP | 2,489 (2,437–2,542) | 0.395 / 0.455 | 45% |
| 512 | Exact read | HTTPS MCP | 3,915 (3,854–4,068) | 0.250 / 0.286 | 71% |
| 4,096 | Append | Native API | 4,347 (4,057–4,455) | 0.223 / 0.268 | 100% |
| 4,096 | Append | Local MCP | 2,207 (2,134–2,227) | 0.444 / 0.520 | 51% |
| 4,096 | Append | HTTPS MCP | 3,382 (3,298–3,458) | 0.287 / 0.347 | 78% |
| 4,096 | Exact read | Native API | 5,254 (5,171–5,455) | 0.185 / 0.211 | 100% |
| 4,096 | Exact read | Local MCP | 2,397 (2,375–2,447) | 0.410 / 0.461 | 46% |
| 4,096 | Exact read | HTTPS MCP | 3,808 (3,768–3,927) | 0.256 / 0.290 | 72% |

## Workload and limits

For the historical three-lane comparison, each application value was a compact JSON object shaped like `{"payload":"…"}`. The byte count includes the braces, property name, quotes, and string, but excludes MCP and HTTPS fields. Each lane made 100 separate append calls, then fetched those exact sequence numbers with 100 separate read calls and checked every returned value. The pool used fast durability, had 64 MiB capacity, and held all messages. Concurrency was one.

The native lane kept one Rust `RemoteClient` alive over the saved connection. Its pinned TLS agent stayed warm across requests, while the client reloaded the saved credential for each call. Local MCP kept one initialized `plasmite mcp --remote` process alive. Direct MCP kept one HTTPS connection alive and sent individual tool calls. Each lane warmed up with an untimed append and exact fetch at each size; MCP clients also completed `initialize` and `notifications/initialized` before timing. All three lanes checked their pool capacity before the timed runs.

All clients and the server ran on the same Mac, so HTTPS used loopback. Both reported Plasmite 0.8.0. The client ran macOS 26.6.2 on arm64 with Rust 1.88.0 and CPython 3.14.7. The direct MCP lane used the `2025-11-25` handshake. This measures request paths without a model. Direct MCP calls ran through a Python HTTP client; native calls ran in a Rust helper, so the table includes the interface work required by each client as well as the server path.

The direct lane remained slower than the native API after the native TLS connection was kept warm. Each lane performed one append or one exact read per operation; the native helper did not batch API operations. The remaining gap reflects the complete client and protocol paths measured in this historical comparison, including MCP tool dispatch and client framing. These figures describe that host and build, not performance over other networks or servers.

## Reproduction

The current harness compares the long-lived native API client with direct HTTPS MCP. It builds the release CLI and a small persistent native API helper. It records operation latencies and sequence numbers, run order, verification counts, versions, machine information, and configuration in JSON. The raw output never includes the access key or OAuth tokens.

The command requires `--server` (an HTTPS origin), `--mcp-url` (the exact `/mcp` URL on that host and port), `--pool`, `--access-key-file`, and `--output`. Optional settings are `--messages`, `--repeats`, `--sizes`, and `--ca-file`. Defaults are 100 messages per phase, five repeats, and payload sizes of 512 and 4,096 bytes. Use at least two repeats and payloads of at least 128 bytes.

Prepare a dedicated pool with enough capacity for the retained messages, a disposable test access key, and a certificate authority (CA) file that lets the test client verify the server. Keep the key in an owner-readable file (`chmod 600`); the harness accepts either a JSON object with an `access_key` field or the key as plain text. For direct MCP, the harness registers a throwaway public client, creates a Proof Key for Code Exchange (PKCE) verifier, requests a grant for the exact `/mcp` resource, and approves it through `/oauth/approve` using the protected key file. It exchanges and revokes the grant itself. Tokens stay in memory. No browser, model provider, or unexplained bearer token is required.

Run the command twice, changing the output path for the second session. These paths keep new measurements separate from the historical files below.

```console
python3 scripts/bench_transport_comparison.py \
  --server "$SERVER_URL" \
  --mcp-url "${SERVER_URL%/}/mcp" \
  --pool transport-bench \
  --access-key-file "$KEY_FILE" \
  --ca-file "$CA_FILE" \
  --output /tmp/transport-comparison-current-1.raw.json
```

The default run performs five repeats at each size, with 100 appends and 100 exact reads per lane in each repeat. For the second session, change the output to `/tmp/transport-comparison-current-2.raw.json`. The historical raw sessions contain ten repeats per condition across the three lanes:

- [First final-profile raw session](transport-comparison-2026-09-26.raw.json)
- [Independent final-profile repeat](transport-comparison-2026-09-26-repeat2.raw.json)
