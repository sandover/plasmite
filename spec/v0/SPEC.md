# Plasmite CLI Spec (v0)

This document defines the normative CLI compatibility contract for v0.
It keeps only script-level guarantees; signatures, walkthroughs, and examples live in code docs and `docs/cookbook.md`.

## Scope

- This spec freezes what scripts and automation can rely on.
- This spec does not freeze internal command wiring, help text prose, or implementation structure.

## Versioning + Compatibility

- The CLI surface is versioned as `v0`.
- Within v0, compatibility is additive-only.
- Existing commands, machine-readable flags, and field meanings must not be removed or redefined.
- New commands/flags/fields may be added when existing behavior remains stable.
- Any breaking change requires a new major version. The secure-sharing release
  is a breaking major-version change with no migration path and removes
  superseded secure connection commands and options.

## Stable Surface

### Frozen v0.0.1 Command Set

- `plasmite pool create`
- `plasmite pool info`
- `plasmite pool list`
- `plasmite pool delete`
- `plasmite feed`
- `plasmite fetch`
- `plasmite follow`
- `plasmite version`

`plasmite duplex` is implemented but not frozen in v0.0.1.

### Machine-Readable Interfaces

- Top-level `--dir`, placed before the command, selects the local pool directory.
- Commands that expose `--json` provide stable machine-readable output through it.
- `fetch` always emits one JSON message envelope.
- `version` emits human text to a TTY and JSON when piped.
- All product-version output uses the same build identity. A clean checkout at
  the matching release tag reports the package version. Other source builds
  append `-dev` and Git commit metadata, plus `.dirty` for changes to tracked
  files. Stage new files to include them in that check.
- Streaming reads provide stable JSON Lines via `--format jsonl` or `--jsonl`.
- `feed` append receipts include `seq`, `time`, and `meta` (not echoed `data`).

### Secure Sharing

YMSGO2 adds native access-key sharing. It keeps the existing spec paths and
`/v0` HTTP route prefix. Local pool use remains credential-free.

- `plasmite --dir DIR serve` starts the server for the selected pool directory.
  `--bind` and `--remote-bind` set listener addresses; `--shared-address`
  names the client-facing HTTPS origin. `--tls-cert` and `--tls-key` supply a
  server certificate; `--front-cert` identifies a TLS proxy's public
  certificate for access-key pinning.
- `plasmite --dir DIR access invite --name NAME` creates a directory-wide
  access key and displays it only after the server commits it.
- `plasmite access connect SERVER_URL` prompts for the access key without
  echoing it, verifies the server, and saves the connection for the current OS
  user. In an interactive macOS or Windows terminal, it then offers browser
  trust for the verified server certificate. It shows the address, all DNS and
  IP subject alternative names, the certificate's SHA-256 fingerprint, expiry,
  and the OS trust-store scope before asking. Declining or failing this step
  leaves the native connection usable. Scripts never wait for this prompt.
- `plasmite access status SERVER_URL` reports whether a connection is saved,
  whether the server is reachable, and whether it accepts the saved access
  secret. On macOS and Windows, it also reports browser trust for the current
  verified certificate and its expiry when that certificate can be reached.
  Windows reports the current certificate's presence in the user's Root store;
  browser and device policies can still reject it. Status never prints credentials.
- `plasmite access untrust SHA256` removes one exact certificate from the
  current user's OS trust store by its 64-character SHA-256 fingerprint. It
  works after certificate renewal or server loss and leaves native credentials
  intact. A replacement certificate needs a new browser trust approval even
  when it keeps the same public key.
- `plasmite access disconnect SERVER_URL` removes this OS user's saved
  credentials without contacting the server. It works while the server is
  unavailable and does not revoke the server key.
- `plasmite access keys` lists server-side key names, IDs, revocation state,
  and approximate recent use; `plasmite access revoke KEY_ID` revokes one key.
- An access key has the form `pk1.<spki-fingerprint>.<secret>`. The
  fingerprint is the 64-character lowercase hexadecimal SHA-256 digest of the
  certificate's DER Subject Public Key Info (SPKI); the secret is 32 random
  bytes encoded as 64 lowercase hexadecimal characters. The address is
  supplied separately.
- Saved connections are selected by destination. Multiple destinations can
  coexist and CLI pool operations use their saved credentials when addressing
  a remote server.
- Repeating `access connect` with the same key is safe. A new key replaces the
  saved credential only after the server accepts it and local storage
  succeeds. A failed connection or save preserves the previous credential.
- A changed address never inherits credentials automatically. Explicit
  reconnection can reuse a key only if the new address presents the same
  pinned public key and accepts that key's secret.
- Local HTTP listens on loopback, defaults to port `9700`, and permits
  credential-free local pool operations. HTTPS defaults to port `9743` and
  requires authentication for remote pool operations. Both ports can be
  configured. Secure serving supports macOS, Linux, and Windows. Windows state
  requires a filesystem that enforces access control lists and restricts access
  to the current user, SYSTEM, and administrators. Startup rejects unsafe
  existing state and paths through junctions or other reparse points.
- `access connect` verifies the destination hostname, certificate validity,
  TLS proof of possession, and the certificate public-key fingerprint from
  the access key before sending the access secret. Redirects must not forward
  credentials to another destination.
- Superseded secure access commands, options, and configuration have no
  compatibility aliases and are rejected.
- Connection and status errors identify the destination and failed step and
  provide an actionable next step. Machine-readable errors have stable kinds;
  background commands do not wait for interactive repair.

## Data + Error Contract

### Message Envelope

- Stable message envelope fields: `seq`, `time`, `meta`, `data`.
- `seq` is monotonic per pool.
- `time` is RFC 3339 UTC text in CLI JSON output.
- `meta.tags` is always present (empty array when unset).
- Message workflows are JSON-in/JSON-out.

### Error + Exit Contract

- Errors are emitted on stderr.
- On TTY stderr: concise human text plus actionable guidance.
- On non-TTY stderr: JSON envelope with required `error.kind` and `error.message`.
- Optional error fields may include `error.hint`, `error.path`, `error.seq`, `error.offset`, `error.causes`.
- Exit-code mapping by error kind is stable for v0 and defined by implementation in `src/core/error.rs`.

## Behavioral Semantics

### Pool Reference Resolution

- `NAME` resolves to `POOL_DIR/NAME.plasmite`.
- Explicit paths (for example `./foo.plasmite` or `/abs/foo.plasmite`) are used as-is.
- Resolution rule:
1. If argument contains `/`, treat as path.
2. Else if it ends with `.plasmite`, resolve as `POOL_DIR/<arg>`.
3. Else resolve as `POOL_DIR/<name>.plasmite`.

### Pool Format Compatibility

- Pool files carry an on-disk format version in the header.
- Incompatible on-disk changes must bump format version.
- Older binaries must refuse newer incompatible formats with actionable guidance.

### Time Filters

- `follow --since` accepts RFC 3339 timestamps and relative values such as `5m`.
- Timestamps before the Unix epoch use zero as their comparison time.

### Platforms

- The frozen v0.0.1 baseline supports macOS and Linux.
- Current official CLI delivery also includes Windows x86_64 through npm and
  PyPI. The current distribution matrix is maintained in
  `docs/record/distribution.md`.

## Non-Contract Surface

The following are implemented but not frozen in v0.0.1 and may evolve within v0:

- `plasmite duplex`
- `plasmite tap`
- `plasmite mcp`
- `plasmite mcp --remote SERVER_URL` uses a saved native HTTPS connection for a local stdio MCP process. MCP methods and messages follow the separately versioned [MCP contract](../mcp/2025-11-25/SPEC.md).
- `plasmite completion`
- `plasmite doctor`
- Remote shorthand refs in CLI commands
- Notice payload details and frequency controls

Current remote shorthand constraints (documented, non-frozen):

- URL refs are explicit remote opt-in in core commands that accept pool refs.
- `tap` currently accepts local pool refs only; URL refs are rejected with an actionable usage hint.
- `duplex` remote refs reject `--create` and `--since`; use `--tail` for remote history.
- `follow` remote refs reject `--since` and `--replay`; use `--tail` for remote history.

## References

- CLI operating model: `docs/cli.md`
- Remote protocol contract: `spec/remote/v0/SPEC.md`
- Public API contract: `spec/api/v0/SPEC.md`
