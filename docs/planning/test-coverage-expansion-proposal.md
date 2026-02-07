<!--
Purpose: Capture test coverage gaps and expansion ideas for recent changes.
Key Exports: N/A (proposal document).
Role: Guide short-term test additions based on last 24 hours of commits.
Invariants: Suggestions should align with current APIs/specs and avoid new behavior.
Notes: Focuses on coverage expansion, not implementation critique.
-->

# Test Coverage Expansion Proposal (Last 24 Hours)

## Scope

Time window reviewed: Feb 3 16:28 - Feb 4 15:49 (local).
Key areas touched in this window:
- Remote HTTP server + client (serve + RemoteClient).
- New API surface and validation reporting.
- Conformance runner + negative + multiprocess manifests.
- Go, Python, and Node bindings plus conformance harnesses.
- C ABI additions and ABI smoke test.
- CLI doctor command and expanded CLI integration tests.

## Current Coverage Snapshot (Observed)

- CLI integration tests cover create/poke/get/peek flows, doctor ok/corrupt, JSON output shapes, permission errors, and stdin pokes.
- Remote integration tests cover append/get/tail ordering and NotFound error propagation across HTTP.
- C ABI has a single smoke test for append/get.
- Conformance manifests cover create/append/get/tail, negative error expectations, and a multiprocess spawn_poke scenario.
- Node/Python tests run conformance manifests; Go has no _test.go files beyond the runner itself.
- Remote client has a few unit tests around URL parsing.

## Gaps And Proposed Additions

### Remote Server + Client (HTTP/JSON)

Coverage gaps:
- Authorization: missing, wrong, and valid bearer tokens.
- Non-loopback bind rejection and related error kinds/messages.
- Endpoints not covered: list_pools, delete_pool, pool_info.
- Error envelope mapping for Usage, AlreadyExists, Busy, Corrupt, Permission, Io, Internal.
- Tail query edge cases: since_seq past newest, max=0, timeout semantics, empty streams.
- Stream robustness: JSONL with blank lines, partial lines, invalid JSON.
- Pool refs with path segments and URI parsing variants.

Proposed tests:
- Integration tests that start serve with --token and validate 401 on missing/invalid token plus success on correct token.
- Integration test for refusing non-loopback bind (e.g., 0.0.0.0) and confirm Usage error kind.
- Remote client tests for list_pools + delete_pool and pool_info fields (bounds, ring_size) via a real server.
- Tail tests for since_seq beyond newest (expect no messages), max_messages=1, and timeout returning None then still resumable.
- Tail cancellation: drop or cancel the client side mid-stream and ensure server thread exits cleanly.
- Remote client error mapping tests by intentionally provoking AlreadyExists, Busy (locked pool), Corrupt, Permission, and Usage errors.

User perspective / misuse to cover:
- Base URL with a path, query, or non-http(s) scheme.
- Pool URI like http://host/v0/pools/a/b (multi-segment path) and trailing slashes.
- Accidental use of remote pool refs via local APIs or vice versa.
- Passing non-UTF-8 pool paths when using PoolRef::path.

### Serve CLI

Coverage gaps:
- CLI flag parsing for serve subcommand (invalid binds, missing args, invalid token format).
- Header behavior: plasmite-version header present on both JSON and JSONL responses.

Proposed tests:
- CLI integration test that launches serve with an invalid --bind and asserts Usage error exit code and message.
- HTTP integration test that checks plasmite-version header for JSON and JSONL responses.

User perspective / misuse to cover:
- Expecting --bind 0.0.0.0 to work (v0 rejects it).
- Passing a token but forgetting to include Authorization header from clients.

### C ABI + Bindings (Go, Python, Node)

Coverage gaps:
- ABI error mapping for each error kind and error message + path/seq/offset metadata.
- Memory safety behaviors for NULL pointers, double free, use-after-free, and empty buffers.
- JSON decoding failures and invalid UTF-8 payload handling.
- Tail streaming behaviors in Go/Node/Python bindings (timeouts, cancellation, EOF).

Proposed tests:
- ABI-level tests that intentionally call functions with invalid inputs and confirm non-zero return codes and error metadata.
- Binding-level tests for Close idempotency and error on using closed handles.
- Binding tests for append/get with large payloads and large tags arrays.
- Streaming tests in Go (context cancellation), Node (abort signal), and Python (generator stop) to verify tail shutdown.

User perspective / misuse to cover:
- Passing numbers vs bigints vs strings for seq/size/timeouts in Node.
- Using bindings without setting PLASMITE_LIB_DIR or PLASMITE_BIN and expecting defaults to work.
- Reusing a pool or client after Close or in multiple threads without synchronization.

### Conformance Manifests + Runners

Coverage gaps:
- No conformance coverage for list_pools, delete_pool, pool_info, or serve/remote semantics.
- No explicit tests for durability=flush or for append timestamp rejection (remote).
- Limited coverage of error metadata fields (path/seq/offset) in negative cases.

Proposed additions:
- Extend manifest schema or add a secondary manifest for pool_info/list/delete operations.
- Add negative cases that validate has_path/has_seq/has_offset for each error kind.
- Add a remote conformance runner (or remote-focused manifest) to validate HTTP/JSON semantics.

User perspective / misuse to cover:
- Expecting conformance to validate CLI outputs; clarify or add CLI-specific manifest tests.
- Running conformance on missing binaries and expecting better diagnostics.

### CLI Doctor

Coverage gaps:
- No coverage for --all behavior or mixed ok/corrupt reporting.
- No coverage for TTY vs JSON output formatting.
- No coverage for NotFound or Permission errors from doctor.

Proposed tests:
- CLI integration test for doctor --all with one ok and one corrupt pool; verify exit code and report list.
- TTY output test using a pseudo-TTY harness (or explicit formatting unit tests if CLI supports it).
- Doctor on missing pool to assert NotFound error and JSON error payload.

User perspective / misuse to cover:
- Running doctor on a directory that is not a pool.
- Expecting doctor --all to skip unreadable pools silently.

### API Surface + Validation

Coverage gaps:
- Limited tests around PoolRef URI parsing and path-based pool refs with slashes.
- ValidationReport edge cases (empty pool, corrupted header, partial corruption).

Proposed tests:
- Unit tests for PoolRef parsing variants (name/path/uri) and error messages.
- ValidationReport tests that validate structured fields for empty pools and corrupt headers.

User perspective / misuse to cover:
- Passing a path that includes globbing or shell-escaped characters in PoolRef::path.
- Expecting API methods to accept non-JSON bytes (explicitly rejected in v0).

## Suggested Priorities

P0:
- Remote auth and error mapping tests.
- ABI invalid input + error metadata tests.
- CLI doctor --all and NotFound coverage.

P1:
- Conformance expansion for list/delete/pool_info + error metadata checks.
- Binding-level tail cancellation and large payload tests.
- Remote tail timeout + since_seq edge tests.

P2:
- TTY formatting tests for CLI outputs.
- Additional PoolRef parsing/normalization edge cases.

## Notes

No implementation changes proposed here; this file is intended as a checklist for test additions only.
