# Serving and Remote Access

This guide covers deploying `plasmite serve` for remote pool access.

For the normative protocol contract, see `spec/remote/v0/SPEC.md`.

## Tap + serving

`plasmite tap` writes messages to local pools. Once those pools exist, `plasmite serve`
exposes them using the same remote read/write behavior as any other pool. No special
server mode is required for tapped pools.

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
plasmite serve init --bind 0.0.0.0:9700 --output-dir ./.plasmite-serve

# 2) Start server with generated artifacts
plasmite serve \
  --bind 0.0.0.0:9700 \
  --allow-non-loopback \
  --token-file ./.plasmite-serve/serve-token.txt \
  --tls-cert ./.plasmite-serve/serve-cert.pem \
  --tls-key ./.plasmite-serve/serve-key.pem
```

`serve init`, `serve check`, and secure startup banners display:

- `tls_fingerprint: SHA256:...`

Use that fingerprint for out-of-band trust verification before sharing client commands.

## Client auth + TLS flags

Prefer native client commands over raw curl:

```bash
# Feed with bearer token file + trusted cert
plasmite feed https://server:9700/events \
  --token-file ./.plasmite-serve/serve-token.txt \
  --tls-ca ./.plasmite-serve/serve-cert.pem \
  '{"sensor":"temp","value":23.5}'

# Follow with same trust/auth material
plasmite follow https://server:9700/events \
  --token-file ./.plasmite-serve/serve-token.txt \
  --tls-ca ./.plasmite-serve/serve-cert.pem \
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
plasmite serve --tls-self-signed --allow-non-loopback --token-file ./serve-token.txt

# Generated cert/key (via serve init)
plasmite serve init
plasmite serve --tls-cert serve-cert.pem --tls-key serve-key.pem

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
- v1 is intentionally minimal: no MCP resource subscriptions and no SSE mode for MCP POST responses.

## Server limits

Configurable via flags:

| Flag | Default | Purpose |
|---|---|---|
| `--max-body-bytes` | 1 MB | Maximum request body size |
| `--max-tail-timeout-ms` | 30 s | Maximum tail stream timeout |
| `--max-tail-concurrency` | 64 | Maximum concurrent tail streams |

## Reverse proxy

When fronting `plasmite serve` with nginx, Caddy, or similar:

- Proxy HTTP and SSE traffic.
- Forward `Authorization` headers.
- Set appropriate timeouts for long-lived tail streams.
- Let the proxy handle TLS termination and use loopback HTTP between proxy and serve when both are on the same host.
