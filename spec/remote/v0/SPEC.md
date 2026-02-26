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

### Message Write/Read

- `POST /v0/pools/{pool}/append` -> success body `{ "message": ... }`.
- `POST /v0/pools/{pool}/append_lite3` (`application/x-plasmite-lite3`) -> `{ "message": ... }`.
- `GET /v0/pools/{pool}/messages/{seq}` -> success body `{ "message": ... }`.
- `GET /v0/pools/{pool}/messages/{seq}/lite3` -> raw Lite3 bytes with `Content-Type: application/x-plasmite-lite3` and `plasmite-seq` header.

### Streaming

- `GET /v0/pools/{pool}/tail` -> JSONL stream (`application/jsonl`).
- `GET /v0/pools/{pool}/tail_lite3` -> Lite3 stream (`application/x-plasmite-lite3-stream`).
- Lite3 tail frame format: `[u64be seq][u64be timestamp_ns][u32be len][len bytes payload]` repeated.

## Data + Error Contract

### Error Envelope

- Error responses use JSON envelope shape: `{ "error": { "kind": "...", "message": "...", ... } }`.
- `error.kind` and `error.message` are required.
- `error.path`, `error.seq`, and `error.offset` are optional.

### Status Mapping

- `200` success
- `400` usage/malformed input
- `401` unauthorized
- `403` forbidden by access mode
- `404` not found
- `409` already exists
- `413` payload too large
- `423` busy/locked
- `500` internal/corrupt/io failures

## Behavioral Semantics

### Authentication + Access

- When auth is enabled, clients send `Authorization: Bearer <token>`.
- Auth failures return `401`.
- Access-mode violations return `403`.

### Pool Naming Rules

- Remote `{pool}` parameters accept pool names only (no path separators).
- Path-based pool resolution is local-only behavior and out of remote v0 scope.

### Streaming Semantics

- Delivery ordering is ascending `seq`.
- Cancellation is by client connection close.
- Reconnect flows are at-least-once; clients should resume via `since_seq` and de-duplicate by `seq`.
- On post-start failure, `/tail` may emit one terminal JSON error-envelope line before close.
- On post-start failure, `/tail_lite3` closes the stream without a JSON body frame.

### Server Limits

- Servers may enforce max request body size (`413`).
- Servers may enforce max tail timeout (`400` when exceeded).
- Servers may cap concurrent tails (`423`).
- Body/size limits should be applied consistently to JSON and Lite3 append paths.

## Non-Contract Surface

Routes outside the stable endpoint set above are not part of the remote v0 compatibility surface.
Examples: `/healthz`, `/ui`, `/v0/ui/...`.

## References

- CLI contract: `spec/v0/SPEC.md`
- Public API contract: `spec/api/v0/SPEC.md`
