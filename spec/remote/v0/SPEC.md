# Plasmite Remote Protocol v0 (HTTP)

This document defines the normative remote protocol contract for v0.
It captures stable wire-level compatibility guarantees only.

## Scope

- This spec freezes endpoint shapes, payload envelopes, and protocol semantics clients depend on.
- This spec does not freeze server internals, UI routes, or implementation-specific optimizations.

## Versioning + Compatibility

- Remote API versioning uses path prefix `/v0/...`.
- Servers include `plasmite-version: 0` in responses.
- Compatibility within v0 is additive-only.
- Existing endpoint meanings and field semantics must not be removed or redefined.
- New optional fields/endpoints may be added without breaking existing clients.

## Stable Surface

### Transport + Encoding

- Transport: HTTP/1.1 or HTTP/2 over TCP.
- JSON request/response encoding: UTF-8.
- JSON streaming encoding: JSON Lines (`application/jsonl`).
- Lite3 byte endpoints are additive performance paths.

### Pool Lifecycle

- `POST /v0/pools` -> success body `{ "pool": ... }`.
- `POST /v0/pools/open` -> success body `{ "pool": ... }`.
- `GET /v0/pools/{pool}/info` -> success body `{ "pool": ... }`.
- `GET /v0/pools` -> success body `{ "pools": [...] }`.
- `DELETE /v0/pools/{pool}` -> success body `{ "ok": true }`.

### Native Access

- `POST /v0/access/invite` accepts `{ "name": "...", "server_fingerprint": "..." }` on the local
  administration listener and returns `200 { "access_key": "..." }`.
  The fingerprint must match the owner state for that listener. The route is
  local-only; remote HTTPS callers cannot create keys.
- `GET /v0/access/keys` is local-only. It requires the current server fingerprint
  in `x-plasmite-server-fingerprint` and returns `{ "keys": [...] }`. Each entry
  has a stable opaque `id`, `name`, `revoked`, `created_at` (Unix seconds, or
  `null` for older records), and `last_used_at` (Unix seconds observed since
  this server started, or `null`). It never returns secrets or
  secret verifiers.
- `POST /v0/access/revoke` is local-only. It accepts
  `{ "id": "...", "server_fingerprint": "..." }` and returns
  `{ "id": "...", "revoked": true }`. Repeating it for a revoked key succeeds;
  an unknown ID returns `404`.
- An access key has the form `pk1.<spki-fingerprint>.<secret>`. The
  fingerprint is the 64-character lowercase hexadecimal SHA-256 digest of
  the certificate's DER Subject Public Key Info (SPKI); the secret is 32
  random bytes encoded as 64 lowercase hexadecimal characters. The server
  stores a SHA-256 verifier of the secret, not the access key or secret
  itself.
- `GET /v0/access/check` requires a valid access secret in
  `Authorization: Bearer <secret>` and returns
  `200 { "accepted": true }`.
- Remote pool operations require HTTPS and the same Bearer authorization.
  Local HTTP remains on loopback and permits credential-free local pool use.
  The default local and secure ports are `9700` and `9743`, respectively;
  both may be configured.

### Browser Session

- The remote HTTPS page accepts a complete `pk1` access key through
  `POST /v0/browser/session` with JSON `{ "access_key": "..." }`. The key appears
  only in the request body. A successful reply sets an opaque `plasmite_session`
  cookie with `Secure`, `HttpOnly`, `SameSite=Strict`, `Path=/v0`, and a 30-day
  maximum age. The server stores only a hash of the session token, the key ID,
  and the expiry in its private serving state. Sessions survive server restart.
- `GET /v0/browser/session` reports whether the cookie remains valid.
  `DELETE /v0/browser/session` removes that session and clears the cookie.
  Logout does not revoke the access key. A session expires after 30 days or
  immediately when its access key is revoked. Every pool operation checks the
  live revocation state. Browser cookies authorize pool operations but not MCP
  or local access administration.
- Browser login, logout, and cookie-authenticated writes require an `Origin`
  matching the remote HTTPS `Host`. The remote listener never serves `/access`.
  Local access administration accepts only loopback `Host` values and rejects
  mismatched `Origin` values. The pages load no third-party scripts and render
  pool data as text.
- A native client checks the destination hostname, certificate validity, TLS
  proof of possession, and the certificate's Subject Public Key Info (SPKI)
  fingerprint against the key before it sends the secret. A redirect must not
  forward the secret to a different destination.

### Message Write/Read

- `POST /v0/pools/{pool}/append` -> success body `{ "message": ... }`.
- `POST /v0/pools/{pool}/append_lite3` (`application/x-plasmite-lite3`) -> `{ "message": ... }`.
- `GET /v0/pools/{pool}/messages/{seq}` -> success body `{ "message": ... }`.
- `GET /v0/pools/{pool}/messages/{seq}/lite3` -> raw Lite3 bytes with `Content-Type: application/x-plasmite-lite3` and `plasmite-seq` header.

### Streaming

- `GET /v0/pools/{pool}/tail` -> JSONL stream (`application/jsonl`).
- `GET /v0/pools/{pool}/tail_lite3` -> Lite3 stream (`application/x-plasmite-lite3-stream`).
- Lite3 tail frame format: `[u64be seq][u64be timestamp_ns][u32be len][len bytes payload]` repeated.
- Tail requests accept `gap_policy=continue|error`; omission means `continue`.
- `/tail_lite3` rejects `gap_policy=error` with `Usage` because Lite3 streams
  have no terminal error frame.

## Data + Error Contract

### Error Envelope

- Error responses use JSON envelope shape: `{ "error": { "kind": "...", "message": "...", ... } }`.
- `error.kind` and `error.message` are required.
- `error.path`, `error.seq`, and `error.offset` are optional.

### Status Mapping

- `200` success
- `400` usage/malformed input
- `401` unauthorized
- `403` forbidden request (for example, a mismatched browser origin)
- `404` not found
- `409` already exists
- `410` retention gap
- `413` payload too large
- `423` busy/locked
- `500` internal/corrupt/io failures

## Behavioral Semantics

### Authentication + Access

- Native HTTPS pool requests send `Authorization: Bearer <secret>` after the
  client verifies the server identity bound into the access key.
- Missing or invalid access secrets return `401`.
- Revocation persists before success is reported. New operations with the key
  fail, and its active tails close without emitting data still queued inside
  Plasmite. Bytes already handed to the transport may arrive. A write admitted
  before revocation may finish; a later write cannot start.
- Each access key grants full pool access throughout the served directory,
  including listing, reading, appending, creating, and deleting pools.
  Mismatched browser origins return `403`. Access administration remains
  local-only.

### Pool Naming Rules

- Remote `{pool}` parameters accept pool names only (no path separators).
- Path-based pool resolution is local-only behavior and out of remote v0 scope.

### Streaming Semantics

- Delivery ordering is ascending `seq`.
- Cancellation is by client connection close.
- Reconnect flows are at-least-once; clients should resume via `since_seq` and de-duplicate by `seq`.
- On post-start failure, `/tail` may emit one terminal JSON error-envelope line before close.
- On post-start failure, `/tail_lite3` closes the stream without a JSON body frame.
- With `gap_policy=error`, `/tail` emits `RetentionGap` before any message after
  a missing sequence; the envelope's `seq` is the first missing sequence.

### Server Limits

- Servers may enforce max request body size (`413`).
- Servers may enforce max tail timeout (`400` when exceeded).
- Servers may cap concurrent tails (`423`).
- Body/size limits should be applied consistently to JSON and Lite3 append paths.
- The server bounds HTTPS connections before authentication and expires
  incomplete TLS handshakes, HTTP/1 headers, and idle request bodies. HTTP/2
  uses keepalive pings to check connection liveness. It releases completed
  connection tasks during normal service.

## Non-Contract Surface

Routes outside the stable endpoint set above are not part of the remote v0 compatibility surface.
Examples: `/healthz`, `/ui`, `/v0/ui/...`, `/mcp`.

`/mcp` is outside the v0 stability contract. Its versioned message, transport,
and authorization contract is in `spec/mcp/2025-11-25/SPEC.md`.

## References

- CLI contract: `spec/v0/SPEC.md`
- Public API contract: `spec/api/v0/SPEC.md`
