<!--
Purpose: Propose a stable, long-lived public API for Plasmite beyond the CLI, suitable for embedding and multi-language bindings.
Exports: N/A (documentation); includes API sketches and test strategy.
Role: Forward-looking TDD/RFC for maintainers; complements the normative CLI spec and informs future spec/binding work.
Invariants: The v0 CLI contract in `spec/v0/SPEC.md` remains the source of truth for scripting; this doc must not silently redefine it.
Notes: This proposal prioritizes a small additive surface area, explicit compatibility rules, and a conformance test suite.
-->

# Plasmite Technical Design Document (Public API) v0 (proposal)

Plasmite v0.0.1 is intentionally **CLI-first**: JSON in/out, stable exit codes, and a durable on-disk pool format.
This TDD proposes a **best-in-class public API** (libraries + bindings) that preserves those properties while enabling:

- embedding Plasmite directly into applications (no subprocess / no CLI parsing),
- safe, ergonomic multi-language usage,
- future remote access (`plasmite serve`) without breaking client code.

This document is non-normative until promoted into a versioned API spec (recommended: `spec/api/v0/SPEC.md`).

---

## Goals

- **One conceptual model, many front-ends**: the same operations work from CLI, Rust library, and other languages.
- **Transport-agnostic clients**: local pools today; remote pools later (without rewriting apps).
- **Minimal stable surface area**: small API, strongly versioned, additive-only within a major.
- **Explicit correctness semantics**: concurrency, ordering, durability, and cancellation are unambiguous.
- **Best-in-class testing**: a cross-language conformance suite plus fuzz/property tests for storage invariants.

## Non-goals (initial v0)

- A full “distributed log” system (partitioning, replication, consumer groups).
- Arbitrary server-side query execution (e.g., “run jq on the server”).
- A bespoke schema system (Avro/Protobuf) baked into the core contract.
- A stable on-disk ABI that exposes Lite³ internals to consumers.

---

## Who is this API for?

Primary users:
- Tooling authors embedding Plasmite as an IPC/logging substrate (build systems, agents, local orchestrators).
- App developers wanting low-latency local event streams without running a daemon.
- SRE/devex teams needing deterministic, script-stable behavior (same semantics as CLI).

Constraints:
- Local-first, multi-process, crash-safe, bounded retention (ring buffer).
- macOS + Linux (v0.0.1); keep Windows as a later design point unless the lock/mmap story is revisited.

---

## API design principles

1. **Stable core nouns**: `PoolRef`, `Pool`, `Message`, `Meta`, `Cursor`.
2. **Two-tier contract**:
   - *Normative*: data model + operation semantics + error kinds + wire formats.
   - *Ergonomic wrappers*: language-idiomatic helpers layered on top (not normative).
3. **Additive changes only** within v0:
   - new fields are optional,
   - new operations are new entrypoints,
   - existing semantics don’t drift.
4. **Make “hard parts” explicit**: timeouts, retries, durability, and cancellation are required parameters (or have sharp defaults).
5. **Interop-first**: every binding can be implemented on top of one narrow, well-tested “spine”.

---

## Conceptual model

### Core types

**PoolRef**
- Identifies a pool, without committing to how it is reached.
- v0 supports:
  - `name("chat")` → resolved in `POOL_DIR`
  - `path("/abs/foo.plasmite")`
- Later: `uri("tcp://host:port/pool/chat")` (or similar).

**Message**
- Mirrors the CLI message schema:
  - `seq: u64`
  - `time: RFC3339 UTC string` (or `SystemTime` in languages that strongly prefer it, but serialized as RFC3339)
  - `meta.descrips: []string`
  - `data: JSON value (object recommended; any JSON allowed if CLI allows it)`
- API must preserve the invariant: `seq` is monotonically increasing per pool.

**Meta**
- Starts with `descrips` (tags).
- Future additive fields must be namespaced or carefully versioned.

**Cursor**
- A durable-ish position for readers, represented as `(pool_uuid, seq)` with best-effort validation.
- v0 can start with `seq` only (simpler), but the type should be future-proofed for pool identity.

### Operations

These are the operations all bindings must support:

- `create_pool(ref, options) -> PoolInfo`
- `open_pool(ref, options) -> Pool`
- `pool_info(ref) -> PoolInfo`
- `list_pools(dir) -> [PoolInfo]` (local only)
- `delete_pool(ref) -> ()`

- `append(pool, AppendRequest) -> Message` (“poke”)
- `get(pool, seq) -> Message`
- `tail(pool, TailRequest) -> Stream<Message>` (“peek”)

Recommended extras (still v0, but can land after the above exists):
- `validate_pool(ref) -> ValidationReport` (library equivalent of a future `plasmite doctor`)
- `compact_pool(ref)` (if compaction is ever real; likely not for ring buffers)

---

## Language coverage strategy

### Tiering

**Tier 0 (canonical)**
- **Rust**: canonical implementation + reference semantics + conformance harness.

**Tier 1 (official, supported)**
- **Go**: strong fit for CLI tooling, agents, and daemons; great concurrency story.
- **Python**: scripting + data workflows; common in build/ML/automation ecosystems.
- **TypeScript/Node**: dev tooling ecosystems; also useful for Electron and CLIs.

**Tier 2 (later, demand-driven)**
- **Java/Kotlin** (Gradle, JVM tooling), **C#** (Unity/dev tools), **Ruby** (ops tooling).

### How to implement multi-language APIs (recommended)

Prefer a single “spine” and build everything on it:

1. **Rust native library** provides the canonical semantics.
2. **C ABI layer** exposes a narrow, stable interface (opaque handles, bytes in/out, callbacks).
3. Bindings (Go/Python/Node) build on the C ABI to avoid re-implementing locking/mmap correctness rules.

Rationale:
- One place to get concurrency + storage invariants correct.
- Conformance suite becomes “one harness, many bindings”.
- Remote access later can be implemented behind the same abstract client interface.

---

## Proposed public API surface (Rust)

### Crate layout (recommended end state)

- `plasmite_core` (internal but well-tested): storage format + locking + validation.
- `plasmite_api` (public): stable types + transport abstraction + ergonomic builders.
- `plasmite_cli` (binary): CLI parsing + JSON formatting + exit code mapping.

This can start as modules inside the existing crate and split later without breaking consumers.

### API sketch (Rust)

```rust
use plasmite_api::{
  AppendOptions, Client, LocalClient, PoolRef, TailOptions,
};

let client = LocalClient::new().with_pool_dir("~/.plasmite/pools")?;
let pool = client.open(PoolRef::name("chat")).create_if_missing(true).open()?;

let msg = pool.append_json(r#"{"from":"alice","msg":"hi"}"#)
  .descrip("important")
  .durability(AppendOptions::FAST)
  .commit()?;

let stream = pool.tail(TailOptions::default().since("5m")?.format_json())?;
for msg in stream {
  println!("{}", msg?);
}
```

Key decisions embedded in this sketch:
- **Client** owns transport + resolution rules; **Pool** owns operations on a specific pool.
- **Append** is structured (options + tags + durability) and returns the committed message envelope.
- **Tail** returns an iterator/stream with cancellation and backpressure semantics.

### Error model (Rust)

Expose a stable error kind enum aligned with CLI exit codes:

- `Usage`
- `NotFound`
- `AlreadyExists`
- `Busy`
- `Permission`
- `Corrupt`
- `Io`
- `Internal`

Each error includes structured context (path, pool ref, seq, offset) and a human hint.
Bindings should map these kinds to idiomatic exceptions/errors while preserving kind + context.

---

## Transport abstraction (local now, remote later)

### `Client` trait (conceptual)

Clients implement the same interface regardless of transport:
- `LocalClient`: resolves `PoolRef::{name,path}` and calls core directly.
- `RemoteClient`: resolves `PoolRef::uri` and speaks a wire protocol to a server.

Design constraints:
- The *client interface* must not expose “local-only” concepts (file descriptors, mmap pointers).
- Streaming must support:
  - cancellation,
  - bounded buffering/backpressure,
  - explicit reconnection semantics (remote).

### Remote compatibility strategy

To keep this API stable, remote support should be delivered as:
- a new `RemoteClient` implementation, plus
- a wire protocol spec,
not as changes to every operation signature.

---

## C ABI (the bindings “spine”)

### Why an ABI at all?

Re-implementing multi-process locking, torn-write recovery, and ring semantics in every language is risky.
A C ABI allows:
- a single canonical implementation,
- stable binary integration for Go/Python/Node,
- the ability to ship a `libplasmite` alongside the CLI.

### Shape (proposal)

Use opaque handles and explicit lifetimes:
- `plsm_client_t*` (local or remote)
- `plsm_pool_t*`
- `plsm_stream_t*`

Use byte buffers for payloads:
- Input: UTF-8 JSON bytes (optionally JSONL/seq/event streams later).
- Output: UTF-8 JSON message envelope bytes (exactly as CLI would print in `--format jsonl`).

Expose structured errors:
- `plsm_error_kind` + `message` + optional structured fields (path/seq/offset).

Avoid callbacks initially; prefer pull-based iteration:
- `plsm_stream_next(stream, &out_bytes, &out_len, &out_err)`

---

## Testing strategy

### 1) Spec-level conformance suite (must-have)

Create a language-agnostic test suite that defines:
- operations (create/open/append/get/tail),
- test vectors (inputs + expected outputs),
- timing bounds for tailing/watch tests,
- required error kinds for failure cases.

Recommended structure:
- `conformance/` directory with JSON test manifests.
- A small runner per language that maps manifest operations to API calls.

The conformance suite should validate at minimum:
- message envelope shape (`seq`, `time`, `meta.descrips`, `data`)
- monotonic `seq` behavior
- durability mode semantics (at least “fast” vs “flush”)
- multi-process writer safety (serialized appends)
- tail/watch behavior (ordering, no duplicates, cancellation)
- stable error kinds for common failures (missing pool, corrupt pool, bad input)

### 2) Storage invariant tests (Rust, internal)

Continue/expand:
- unit tests around frame layout, torn-write recovery, overwrite safety,
- property tests (proptest) for random sequences of appends/overwrites,
- fuzzing (cargo-fuzz) for parser/validator entrypoints and corrupt inputs.

### 3) Integration tests (CLI parity)

Add “API parity” tests that:
- perform operations via the Rust API,
- then observe results via CLI `plasmite get/peek`,
ensuring the CLI contract remains intact as internals evolve.

### 4) Performance tests (non-gating, but tracked)

Maintain a small benchmark matrix:
- append throughput (single writer / N writers),
- tail latency (steady-state, worst-case),
- CPU + RSS overhead for streams.

Track baselines in `docs/perf.md` with “budget” guidance (not hard gates).

---

## Versioning & compatibility policy

### What is “v0” for the public API?

Within v0:
- additive changes only,
- no breaking renames/removals,
- conformance suite remains stable and only grows.

Breaking changes require:
- new major (v1),
- a migration guide,
- parallel support window if feasible.

### Contracts

- **CLI contract**: remains normative for scripting (`spec/v0/SPEC.md`).
- **Public API contract**: becomes normative once `spec/api/v0/SPEC.md` exists and is referenced by bindings.
- **ABI contract**: versioned separately (e.g. `libplasmite.so.0`), with symbol versioning where available.

---

## v0 deliverables vs roadmap

### v0 (public API) deliverables

1. **Rust public API** (local-only) with stable types and error kinds.
2. **Conformance test suite** (language-agnostic manifests + Rust runner).
3. **C ABI** (local-only) sufficient to implement one binding.
4. **One official binding** (recommended: Go) to validate the approach end-to-end.
5. **Documentation**: “Getting started with the API” + “Compatibility and guarantees”.

### Roadmap (post-v0)

- Additional official bindings (Python, TypeScript/Node).
- Remote transport (`plasmite serve`) + wire protocol spec.
- `plasmite doctor` + library validator APIs.
- Optional per-entry checksums and richer diagnostics (still additive to message envelope).
- Windows support (requires an explicit design pass over mmap + locking + file watching).

---

## Open questions

- Should `data` be restricted to JSON objects in the *library* even if the CLI accepts any JSON value?
- Do we standardize a cross-language “where” filter language, or keep filtering client-side in bindings?
- What is the minimum acceptable remote semantics for `tail` (at-least-once vs exactly-once) when a connection drops?

---

## Binary data support (proposal)

Plasmite is JSON-first at the contract boundary. For long-term compatibility across languages, “binary support” should be delivered as **standardized conventions** layered on top of the existing `{meta,data}` envelope.

### Design goals

- Preserve the v0 “inspectable JSON” story (CLI output remains JSON / JSONL).
- Make small binary payloads ergonomic in all official bindings.
- Provide a scalable path for large payloads without turning pools into artifact stores.
- Keep the on-disk canonical payload shape stable (no special “binary frame” format in v0).

### Tier 1: Inline bytes (small payloads)

Represent bytes as base64 within `data`, using a stable mini-schema:

```json
{
  "type": "blob",
  "content_type": "application/octet-stream",
  "encoding": "base64",
  "bytes_base64": "AAEC…",
  "len": 3,
  "sha256": "…"
}
```

Guidance:
- `len` is the decoded byte length (not base64 string length).
- `sha256` is optional but strongly recommended for integrity and dedupe.
- Bindings should provide `append_bytes(bytes, content_type, opts)` and `message.try_bytes()` helpers that:
  - validate base64,
  - enforce size limits,
  - optionally compute/verify `sha256`.

### Tier 2: Chunking (medium payloads)

When the payload is too large for comfortable inline base64, standardize chunking:

```json
{
  "type": "blob_chunk",
  "blob_id": "sha256:…",
  "chunk_index": 0,
  "chunk_count": 10,
  "encoding": "base64",
  "bytes_base64": "…"
}
```

Rules:
- `blob_id` should be stable and content-addressed (`sha256:<hex>`), so readers can detect duplicates.
- Readers must tolerate missing chunks (bounded retention can overwrite); APIs should surface “incomplete blob” distinctly from storage corruption.

### Tier 3: Out-of-band blobs (large payloads) (roadmap)

For large artifacts, pools should carry references and metadata, not bytes:

```json
{
  "type": "blob_ref",
  "blob_id": "sha256:…",
  "len": 1234567,
  "content_type": "application/zip"
}
```

This implies a local blob store (e.g. `~/.plasmite/blobs/<sha256>`) and/or remote retrieval when remote pools exist.
This keeps pools focused on event streams while still enabling rich payloads.

### Conformance tests for binary conventions

Extend the conformance suite with:
- Inline blob round-trip (bytes → message → bytes), including `len` and optional `sha256`.
- Invalid base64 handling and size-limit enforcement.
- Chunk assembly correctness (including missing/duplicate/out-of-order chunks).
- Clear error kinds for “incomplete blob” vs “corrupt pool” vs “usage error”.
