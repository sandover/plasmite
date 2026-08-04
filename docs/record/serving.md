# Serving and Remote Access

This guide covers deploying `plasmite serve` for remote pool access.

For the normative protocol contract, see `spec/remote/v0/SPEC.md`.

## Tap + serving

`plasmite tap` writes messages to local pools. Once those pools exist, `plasmite serve`
exposes them using the same remote read/write behavior as any other pool. No special
server mode is required for tapped pools.

## Choose the network boundary

Plasmite supports three practical arrangements. Choose based on who can see the
traffic between client and server.

| Arrangement | Plasmite setup | What protects the traffic |
| --- | --- | --- |
| Same machine | Loopback, no token, no TLS | The operating system keeps loopback traffic on the machine. |
| Private link | Network bind, bearer token, optional `--insecure-no-tls` | An outside layer such as a host-only virtual-machine network, virtual private network, or encrypted tunnel. |
| Untrusted network | Network bind, bearer token, TLS | Plasmite encrypts traffic and proves the server identity with its certificate. |

`--insecure-no-tls` is an explicit exception, not a general remote default. A
bearer token controls access but does not hide itself or pool data from someone
who can observe plaintext traffic. Use this mode only when another layer keeps
that traffic private.

These arrangements do not change how clients address pools. Local clients use
pool names; remote clients use pool URLs. `feed`, `follow`, and `/mcp` retain the
same behavior.

## Quick local start

```bash
plasmite serve                                # loopback, no auth, no TLS
```

## Bind and network access

By default, `plasmite serve` binds to `127.0.0.1:9700` (loopback only).

To listen on all interfaces, use `--bind` with `--allow-non-loopback`:

```bash
plasmite serve --bind 0.0.0.0:9700 --allow-non-loopback
```

Non-loopback + write access requires both `--token-file` and TLS (unless `--insecure-no-tls` is explicitly used for demos).

## Secure remote bootstrap (recommended)

Generate artifacts once, then run the server with those artifacts:

```bash
# 1) Generate token + cert + key + client command scaffolding
plasmite serve init --bind 0.0.0.0:9700 --host pools.example.com --output-dir ./.plasmite-serve

# 2) Start server with generated artifacts
plasmite serve \
  --bind 0.0.0.0:9700 \
  --allow-non-loopback \
  --token-file ./.plasmite-serve/plasmite-auth-token.txt \
  --tls-cert ./.plasmite-serve/plasmite-tls-cert.pem \
  --tls-key ./.plasmite-serve/plasmite-tls-key.pem
```

`serve init`, `serve check`, and secure startup banners display:

- `tls_fingerprint: SHA256:...`

Use that fingerprint for out-of-band trust verification before sharing client commands.

`serve init` generates every requested credential before replacement and
serializes initializers that share an output directory. Replacement uses
private staging files, recoverable backups, and a small transaction journal.
An ordinary write or rename failure restores the prior complete set. If the
process is interrupted during replacement, the next `serve init` for that
directory restores the prior set before doing new work. This is crash recovery,
not a claim that separate filesystem paths change in one atomic operation.

Generated tokens and private keys are owner-only: mode `0600` in directories
with mode `0700` on Unix, and a protected owner-only access-control list on
Windows. Windows uses the built-in `icacls` tool to remove inherited access and
grant the creating account full control; serving inspects the effective ACL
with PowerShell and refuses access by ordinary accounts other than the creator.
Windows `SYSTEM` and built-in Administrators remain permitted because those
privileged identities can take ownership regardless.

## Client auth + TLS flags

Prefer native client commands over raw curl:

```bash
# Feed with bearer token file + trusted cert
plasmite feed https://server:9700/events \
  --token-file ./.plasmite-serve/plasmite-auth-token.txt \
  --tls-ca ./.plasmite-serve/plasmite-tls-cert.pem \
  '{"sensor":"temp","value":23.5}'

# Follow with same trust/auth material
plasmite follow https://server:9700/events \
  --token-file ./.plasmite-serve/plasmite-auth-token.txt \
  --tls-ca ./.plasmite-serve/plasmite-tls-cert.pem \
  --tail 20
```

`--tls-skip-verify` exists for development-only scenarios where full trust bootstrapping is not available yet:

```bash
plasmite follow https://server:9700/events --tail 20 --tls-skip-verify
```

Treat `--tls-skip-verify` as unsafe and temporary.

curl remains useful for API debugging, but should be secondary for operator workflows.

## TLS modes

Three options, from easiest to most controlled:

```bash
# Self-signed (development / demos)
plasmite serve --tls-self-signed --allow-non-loopback --token-file ./plasmite-auth-token.txt

# Generated cert/key (via serve init)
plasmite serve init
plasmite serve --tls-cert plasmite-tls-cert.pem --tls-key plasmite-tls-key.pem

# Bring your own cert
plasmite serve --tls-cert /etc/letsencrypt/live/pool.example.com/fullchain.pem \
               --tls-key /etc/letsencrypt/live/pool.example.com/privkey.pem
```

## Access modes

Control read/write permissions with `--access`:

```bash
plasmite serve --access read-only   # safe for public-facing or browser demos
plasmite serve --access write-only  # ingest-only endpoint
plasmite serve --access read-write  # default
```

## CORS (browser access)

If a web page is served from a different origin than `plasmite serve`, the browser
blocks cross-origin requests unless the server explicitly allows the page origin.

Use repeatable `--cors-origin` flags with exact origins:

```bash
plasmite serve --access read-only \
  --cors-origin https://demo.example.com \
  --cors-origin https://staging.example.com
```

Rules:

- Exact origins only (`scheme://host[:port]`). Wildcards are rejected.
- If the page is HTTPS, the serve endpoint must also be HTTPS.
- Prefer a backend relay if you need secret credentials in the browser.

## Browser UI endpoints

`plasmite serve` includes a built-in UI at `/ui`. Key API endpoints for browser integrations:

- `GET /v0/ui/pools` — list pools
- `GET /v0/ui/pools/<pool>/events` — SSE stream for one pool

## MCP endpoint (`/mcp`, experimental)

`plasmite serve` also exposes an experimental MCP endpoint at `/mcp`.

Transport profile in v1:
- `POST /mcp` accepts exactly one JSON-RPC message.
- JSON-RPC requests return one JSON-RPC response with `Content-Type: application/json`.
- Accepted JSON-RPC notifications/responses return `202 Accepted` with no body.
- `GET /mcp` returns `405 Method Not Allowed`.

Protocol and header notes:
- `MCP-Protocol-Version` is optional in v1.
- If `MCP-Protocol-Version` is present, supported value is `2025-11-25`; invalid/unsupported values return `400`.
- If `Origin` is present and syntactically invalid, request is rejected with `403`.

Security and policy posture:
- `/mcp` uses the same bearer auth and TLS expectations as `/v0/*`.
- `--access` mode restrictions apply to MCP operations.
- Tool discovery is access-aware: read-only servers list only read tools,
  write-only servers list only write tools, and read-write servers list both.
  Authorization is still enforced when each tool executes.
- MCP tool definitions publish standard behavior annotations. Pool listing,
  metadata, fetch, read, and bounded wait operations are read-only and
  idempotent; feed and create operations are mutating; pool deletion is
  destructive. Clients may use these hints when deciding whether a tool call
  needs approval.
- Tool definitions include JSON output schemas for both successful structured
  results and structured tool errors. Initialization instructions explain the
  pool/message model, bounded retention, safe first action, ordinary read/wait
  behavior, advanced cursor usage, and feed retry safety.
- Sequence numbers are automatic metadata. Ordinary MCP list, feed, read, and
  wait workflows do not require callers to provide or manage them. Exact fetch
  and resumable delivery expose sequence numbers as advanced controls.
- Creating an existing pool returns `AlreadyExists` without changing it; MCP
  guidance directs callers to use the preserved pool as-is.
- MCP tag filters require all specified tags. jq `where` filtering is not
  advertised because it is not implemented on the MCP surface.
- MCP pool resources describe capacity and recent-message behavior without
  exposing transient sequence bounds. Reading a pool resource returns up to the
  latest 20 retained messages.
- `plasmite_wait` waits up to `timeout_ms` (default 10 seconds, maximum 60
  seconds). Without `after_seq`, it snapshots the pool's current end and waits
  only for later messages, like a live tail. With `after_seq`, it first catches
  up from that cursor. It returns the same ascending message batch and cursor
  metadata as `plasmite_read`, plus `timed_out` so idle agents do not need
  shell-based sleeps.
- Read and wait results set `next_after_seq` to the highest sequence examined,
  even when filters return no messages. `last_returned_seq` identifies the
  last match. `oldest_available_seq`, `newest_available_seq`, and
  `fell_behind` expose retention bounds and cursor gaps.
- MCP waits share the `--max-tail-concurrency` budget with HTTP tail streams.
  Calls beyond that limit return a structured `Busy` tool error.
- v1 is intentionally minimal: no MCP resource subscriptions and no SSE mode for MCP POST responses.

## Server limits

Configurable via flags:

| Flag | Default | Purpose |
|---|---|---|
| `--max-body-bytes` | 1 MB | Maximum request body size |
| `--max-tail-timeout-ms` | 30 s | Maximum HTTP tail-stream timeout |
| `--max-tail-concurrency` | 64 | Maximum concurrent tail streams and MCP waits |

MCP waits have their own fixed maximum of 60 seconds; they share the concurrency
budget but not `--max-tail-timeout-ms`.

The server also uses `--max-tail-concurrency` as the admission limit for
ordinary pool operations such as create, list, append, and get. These
synchronous filesystem and memory-map operations run outside Tokio's async
runtime workers. When all admitted operations are active, another ordinary
storage request fails immediately with the stable `Busy` response (HTTP 423)
and can be retried after an in-flight operation completes. The server does not
build an unbounded storage queue.

Long-lived HTTP tail streams and MCP waits use their existing specialized
budget. Health checks and MCP request routing remain responsive while ordinary
storage work is active or saturated.

## Reverse proxy

When fronting `plasmite serve` with nginx, Caddy, or similar:

- Proxy HTTP and SSE traffic.
- Forward `Authorization` headers.
- Set appropriate timeouts for long-lived tail streams.
- Let the proxy handle TLS termination and use loopback HTTP between proxy and serve when both are on the same host.
