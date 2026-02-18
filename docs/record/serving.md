# Serving and Remote Access

This guide covers deploying `pls serve` — the HTTP/JSON server for remote pool access.

For the normative protocol contract, see `spec/remote/v0/SPEC.md`.

## Quick local start

```bash
pls serve                                # loopback, no auth, no TLS
```

## Bind and network access

By default, `pls serve` binds to `127.0.0.1:9700` (loopback only).

To listen on all interfaces, use `--bind` with `--allow-non-loopback`:

```bash
pls serve --bind 0.0.0.0:9700 --allow-non-loopback
```

Non-loopback + write access requires both a `--token-file` and TLS (see below).

## TLS

Three options, from easiest to most controlled:

```bash
# Self-signed (development / demos)
pls serve --tls-self-signed --allow-non-loopback

# Generated cert/key (via serve init)
pls serve init              # writes cert.pem + key.pem
pls serve --tls-cert cert.pem --tls-key key.pem

# Bring your own cert
pls serve --tls-cert /etc/letsencrypt/live/pool.example.com/fullchain.pem \
          --tls-key /etc/letsencrypt/live/pool.example.com/privkey.pem
```

`--insecure-no-tls` skips TLS for non-loopback write access (not recommended).

## Authentication

Auth is via bearer tokens. Generate a token file with `pls serve init`:

```bash
pls serve --token-file .plasmite-token
# Clients: curl -H "Authorization: Bearer $(cat .plasmite-token)" ...
```

Loopback-only deployments may omit the token.

## Access modes

Control read/write permissions with `--access`:

```bash
pls serve --access read-only   # safe for public-facing or browser demos
pls serve --access write-only  # ingest-only endpoint
pls serve --access read-write  # default
```

## CORS (browser access)

If a web page is served from a different origin than `pls serve`, the browser
blocks cross-origin requests unless the server explicitly allows the page origin.

Use repeatable `--cors-origin` flags with exact origins:

```bash
pls serve --access read-only \
  --cors-origin https://demo.example.com \
  --cors-origin https://staging.example.com
```

Rules:
- Exact origins only (`scheme://host[:port]`). Wildcards are rejected.
- If the page is HTTPS, the serve endpoint must also be HTTPS.
- Prefer a backend relay if you need secret credentials in the browser.

### Troubleshooting CORS

1. **CORS blocked** — confirm `--cors-origin` exactly matches the page origin (scheme + host + port).
2. **Mixed content blocked** — page is HTTPS but serve is HTTP; add TLS.
3. **401 on API calls** — token missing; use `fetch` with custom headers instead of `EventSource` if auth is needed.
4. **Headers look correct but still blocked** — inspect `Access-Control-Allow-Origin` and preflight `OPTIONS` response in browser devtools.

## Browser UI endpoints

`pls serve` includes a built-in UI at `/ui`. Key API endpoints for browser integrations:

- `GET /v0/ui/pools` — list pools
- `GET /v0/ui/pools/<pool>/events` — SSE stream for one pool

## Server limits

Configurable via flags:

| Flag | Default | Purpose |
|---|---|---|
| `--max-body-bytes` | 1 MB | Maximum request body size |
| `--max-tail-timeout-ms` | 30 s | Maximum tail stream timeout |
| `--max-tail-concurrency` | 64 | Maximum concurrent tail streams |

## Reverse proxy

When fronting `pls serve` with nginx, Caddy, or similar:

- Proxy HTTP and SSE traffic.
- Forward `Authorization` headers.
- Set appropriate timeouts for long-lived tail streams.
- Let the proxy handle TLS termination and use `--insecure-no-tls` on the backend if on the same host.
