<!--
Purpose: Define the v0 conformance manifest format for Plasmite bindings.
Exports: N/A (specification text).
Role: Normative schema for manifests consumed by conformance runners.
Invariants: Versioned schema; step order is significant and deterministic.
Notes: Manifests are JSON to keep parsing consistent across languages.
-->

# Conformance Manifest v0

## Top-Level Fields

- `conformance_version` (number, required): Must be `0`.
- `name` (string, required): Human-friendly name for the manifest.
- `workdir` (string, optional): Relative working directory name (default: `work`).
- `steps` (array, required): Ordered list of operations.

## Step Format

Each step is a mapping with these fields:

- `op` (string, required): Operation name.
- `id` (string, optional): Unique identifier for cross-step references.
- `pool` (string, optional): Pool name or path (required for pool operations).
- `input` (object, optional): Operation-specific inputs.
- `expect` (object, optional): Operation-specific expectations.

## Operations (v0)

### `create_pool`

- `pool` (required): Pool name.
- `input.size_bytes` (optional): Pool size in bytes.
- `expect.created` (optional, bool): Defaults to `true`.

### `append`

- `pool` (required).
- `input.data` (required): JSON payload.
- `input.descrips` (optional): Array of tags.
- `expect.seq` (optional): Expected sequence number.
- `expect.error` (optional): Expected error object (see Error Expectations).

### `get`

- `pool` (required).
- `input.seq` (required): Sequence number.
- `expect.data` (required): JSON payload.
- `expect.descrips` (optional): Array of tags.
- `expect.error` (optional): Expected error object (see Error Expectations).

### `tail`

- `pool` (required).
- `input.since_seq` (optional): Start sequence.
- `input.max` (optional): Max messages to read.
- `expect.messages` (required): Array of messages with `data` and optional `descrips`.
- `expect.messages_unordered` (optional): Array of messages without ordering guarantees.
- `expect.error` (optional): Expected error object (see Error Expectations).
`expect.messages` and `expect.messages_unordered` are mutually exclusive.

### `list_pools`

- No `pool` field required.
- `expect.names` (optional): Array of pool names expected to exist.
- `expect.error` (optional): Expected error object (see Error Expectations).

### `pool_info`

- `pool` (required).
- `expect.file_size` (optional): Expected file size in bytes.
- `expect.ring_size` (optional): Expected ring size in bytes.
- `expect.bounds` (optional): Object with `oldest`/`newest` (number or null).
- `expect.error` (optional): Expected error object (see Error Expectations).

### `delete_pool`

- `pool` (required).
- `expect.error` (optional): Expected error object (see Error Expectations).

### `spawn_poke`

- `pool` (required).
- `input.messages` (required): Array of message objects with `data` and optional `descrips`.
- Spawns separate processes that append concurrently via the CLI (`plasmite poke`).
- Runners may honor `PLASMITE_BIN` to locate the CLI binary.

### `corrupt_pool_header`

- `pool` (required): Pool name or path.
- Corrupts the pool header to simulate a `Corrupt` error.

### `chmod_path`

- `input.path` (required): Path to chmod.
- `input.mode` (required): Octal string like `"000"`.
- Only supported on unix runners.

## Error Expectations

When `expect.error` is present, the operation must fail and match:

- `kind` (required): `Usage|NotFound|AlreadyExists|Busy|Permission|Corrupt|Io|Internal`
- `message_contains` (optional): substring match
- `has_path` (optional): boolean
- `has_seq` (optional): boolean
- `has_offset` (optional): boolean

## Runner Requirements

- Steps must execute **in order**.
- If an `expect` field is present, it must be enforced exactly.
- Failure should include the step index and `id` (if present).
- Runners may add transport-specific setup but must not alter semantics.
