<!--
Purpose: Define the v0 remote protocol for Plasmite over HTTP/JSON.
Exports: N/A (specification text).
Role: Normative contract for remote servers and clients.
Invariants: Message envelopes match spec/v0/SPEC.md; error kinds are stable.
Notes: This spec is additive-only within v0.
-->

# Plasmite Remote Protocol v0 (HTTP/JSON)

## Overview

The v0 remote protocol exposes Plasmite operations over HTTP with JSON request/response bodies.
It is designed to be human-friendly and easy to integrate with existing infrastructure.

- Transport: HTTP/1.1 or HTTP/2 over TCP.
- Encoding: UTF-8 JSON bodies; streaming uses JSON Lines (one JSON object per line).
- Auth: bearer tokens via `Authorization: Bearer <token>`.
- Versioning: URI path version prefix (e.g. `/v0`) + `plasmite-version: 0` response header.

## Version Negotiation

- Clients MUST send requests to a versioned path (e.g. `/v0/...`).
- Servers MUST include `plasmite-version: 0` in responses.
- If a server does not support the requested version, it MUST return `404` or `426`.

## Error Envelope

Errors are returned with HTTP status `4xx/5xx` and a JSON body:

```json
{
  "error": {
    "kind": "NotFound",
    "message": "pool not found",
    "path": "/data/pools/foo.plasmite",
    "seq": 42,
    "offset": 0
  }
}
```

- `kind` is required and matches API error kinds.
- `message` is required.
- `path`, `seq`, and `offset` are optional.

## Pool References

- `pool` fields accept **names only** (no path separators).
- Path-based pools are a local implementation detail and are not part of the remote v0 contract.
- Remote URIs are out of scope for v0.

## Endpoints

### Create Pool

`POST /v0/pools`

Request:

```json
{ "pool": "docs", "size_bytes": 67108864 }
```

Response (200):

```json
{
  "pool": {
    "name": "docs",
    "path": "/.../docs.plasmite",
    "file_size": 67108864,
    "ring_offset": 4096,
    "ring_size": 67104768,
    "bounds": { "oldest": 1, "newest": 42 }
  }
}
```

### Open Pool

`POST /v0/pools/open`

Request:

```json
{ "pool": "docs" }
```

Response (200): same `pool` object as create.

### Pool Info

`GET /v0/pools/{pool}/info`

Response (200): same `pool` object as create.

### List Pools

`GET /v0/pools`

Response (200):

```json
{ "pools": [ { "name": "docs", "path": "...", "file_size": 67108864, "ring_offset": 4096, "ring_size": 67104768, "bounds": {} } ] }
```

### Delete Pool

`DELETE /v0/pools/{pool}`

Response (200): `{ "ok": true }`

### Append

`POST /v0/pools/{pool}/append`

Request:

```json
{ "data": {"kind":"note"}, "descrips": ["note"], "durability": "fast" }
```

Response (200):

```json
{ "message": { "seq": 1, "time": "...", "meta": {"descrips": []}, "data": {"kind":"note"} } }
```

### Get

`GET /v0/pools/{pool}/messages/{seq}`

Response (200): same `message` object as append.

### Tail (streaming)

`GET /v0/pools/{pool}/tail?since_seq=1&max=10&timeout_ms=500`

Response (200):

- `Content-Type: application/jsonl`
- Body is a stream of JSON objects (one per line), each matching the `message` envelope.
- When `max` is reached or `timeout_ms` elapses, the server closes the stream.

Cancellation:
- Clients cancel by closing the connection.

Reconnect semantics:
- At-least-once delivery; clients should resume with `since_seq` and de-dupe by `seq`.

## Authentication

- If auth is enabled, clients MUST send `Authorization: Bearer <token>`.
- On auth failure, servers MUST return `401` with an error envelope.
- Loopback-only deployments MAY disable auth by default.

## Status Codes

- `200` success.
- `400` usage errors (malformed input).
- `401` unauthorized.
- `403` forbidden (access mode disallows the operation).
- `404` not found.
- `409` already exists.
- `423` busy/locked.
- `500` internal errors.

## Compatibility

- v0 is additive-only; new fields are optional and must not break existing clients.
- Errors preserve stable kinds and include optional context.
- Message envelopes match `spec/v0/SPEC.md`.
