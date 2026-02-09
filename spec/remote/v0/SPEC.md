# Plasmite Remote Protocol v0 (HTTP/JSON)

## Overview

The v0 remote protocol exposes Plasmite operations over HTTP with JSON request/response bodies.
It is designed to be human-friendly and easy to integrate with existing infrastructure.

- Transport: HTTP/1.1 or HTTP/2 over TCP.
- Encoding: UTF-8 JSON bodies; streaming uses JSON Lines (one JSON object per line).
- Auth: bearer tokens via `Authorization: Bearer <token>`.
- Versioning: URI path version prefix (e.g. `/v0`) + `plasmite-version: 0` response header.
- Lite3-bytes endpoints are an additive performance path; errors remain JSON envelopes.

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
{ "data": {"kind":"note"}, "tags": ["note"], "durability": "fast" }
```

Response (200):

```json
{ "message": { "seq": 1, "time": "...", "meta": {"tags": []}, "data": {"kind":"note"} } }
```

### Append Lite3 (bytes)

`POST /v0/pools/{pool}/append_lite3`

Request:

- `Content-Type: application/x-plasmite-lite3`
- Body: raw Lite3 bytes as defined by `spec/lite3/SPEC.md`.
- Optional query: `durability=fast|flush` (default `fast`).

Response (200): same `message` object as append.

Errors:

- `400` (Usage) for malformed Lite3 payloads or invalid framing.
- `422` (Corrupt) for payloads that fail internal validation after decode.
- Error responses use the standard JSON error envelope.

### Get

`GET /v0/pools/{pool}/messages/{seq}`

Response (200): same `message` object as append.

### Get Lite3 (bytes)

`GET /v0/pools/{pool}/messages/{seq}/lite3`

Response (200):

- `Content-Type: application/x-plasmite-lite3`
- Body: raw Lite3 bytes for the stored message.
- `plasmite-seq` response header with the message sequence number (string).
- Error responses use the standard JSON error envelope.

### Tail (streaming)

`GET /v0/pools/{pool}/tail?since_seq=1&max=10&timeout_ms=500`

Response (200):

- `Content-Type: application/jsonl`
- Body is a stream of JSON objects (one per line), each matching the `message` envelope.
- When `max` is reached or `timeout_ms` elapses, the server closes the stream.

### Tail Lite3 (streaming bytes)

`GET /v0/pools/{pool}/tail_lite3?since_seq=1&max=10&timeout_ms=500`

Response (200):

- `Content-Type: application/x-plasmite-lite3-stream`
- Body is a stream of framed entries until EOF:
  - `[u64be seq][u64be timestamp_ns][u32be len][len bytes payload]` repeated.
  - `seq` and `timestamp_ns` are the stored message metadata.
  - `payload` is the raw Lite3 message bytes (same bytes as in storage).
- When `max` is reached or `timeout_ms` elapses, the server closes the stream.
- Error responses use the standard JSON error envelope (not streamed).

Cancellation:
- Clients cancel by closing the connection.

Reconnect semantics:
- At-least-once delivery; clients should resume with `since_seq` and de-dupe by `seq`.

## Server Limits

- Servers MAY enforce a maximum request body size and return `413` with an error envelope.
- Servers MAY omit the error envelope for `413` responses emitted by transport layers.
- Servers MAY enforce a maximum tail timeout; requests over the limit should return `400` (Usage).
- Servers MAY cap concurrent tail streams; excess requests should return `423` (Busy).
- Servers SHOULD apply the same size limits to `append_lite3` payloads.

## Authentication

- If auth is enabled, clients MUST send `Authorization: Bearer <token>`.
- On auth failure, servers MUST return `401` with an error envelope.
- Loopback-only deployments MAY disable auth by default.

## Access Modes

- Servers MAY be configured read-only or write-only.
- Disallowed operations MUST return `403` with an error envelope.

## Status Codes

- `200` success.
- `400` usage errors (malformed input).
- `401` unauthorized.
- `403` forbidden (access mode disallows the operation).
- `404` not found.
- `409` already exists.
- `413` payload too large.
- `423` busy/locked.
- `500` internal errors.

## Compatibility

- v0 is additive-only; new fields are optional and must not break existing clients.
- Errors preserve stable kinds and include optional context.
- Message envelopes match `spec/v0/SPEC.md`.
