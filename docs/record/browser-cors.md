# Browser CORS Access Guide

Use this guide when your web page is served from a different origin than `pls serve`.

Example:
- web app: `https://demo.wratify.ai`
- pool server: `https://poolbox-1.wratify.ai`

## What CORS means here

Your browser will only let JavaScript read responses from `pls serve` if the server
explicitly allows the page origin.

`pls serve` supports this with repeatable `--cors-origin` flags.

## Recommended server setup

Use explicit allowlisted origins and read-only access for browser-facing demos:

```bash
pls serve \
  --bind 0.0.0.0:9100 \
  --allow-non-loopback \
  --access read-only \
  --cors-origin https://demo.wratify.ai
```

For multiple web origins, repeat the flag:

```bash
pls serve ... \
  --cors-origin https://demo.wratify.ai \
  --cors-origin https://staging-demo.wratify.ai
```

## Browser API surface

For a browser UI, these are the key endpoints:
- `GET /v0/ui/pools` (list available pools)
- `GET /v0/ui/pools/<pool>/events` (SSE stream for one pool)

## Security and deployment rules

1. Use exact origins only: `scheme://host[:port]`.
2. Wildcard origins are intentionally rejected.
3. If the page is HTTPS, the pool endpoint must also be HTTPS.
4. Avoid embedding long-lived bearer tokens in public frontend code.
5. Prefer a backend relay/proxy if you need secret credentials.

## Troubleshooting

1. Browser says CORS blocked:
- Confirm `--cors-origin` exactly matches the page origin.
- Check scheme and port mismatch (for example `http` vs `https`).

2. Browser says mixed content blocked:
- Your page is HTTPS but pool server is HTTP.
- Move pool server to HTTPS.

3. `401` on API calls:
- Token is missing/invalid, or app is calling with no auth header.
- For auth in browsers, use `fetch`-based streaming instead of `EventSource` if you need custom headers.

4. `serve check` looks valid but browser still blocked:
- Inspect response headers in browser devtools:
  - `Access-Control-Allow-Origin`
  - preflight response to `OPTIONS` when auth headers are present.
