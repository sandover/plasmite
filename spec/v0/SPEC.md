# Plasmite CLI Spec (1.x)

This document defines the normative CLI compatibility contract for Plasmite 1.x.
The historical `spec/v0/` path is retained for existing documentation links;
it does not preserve the superseded pre-1.0 CLI behavior.
It keeps only script-level guarantees; signatures, walkthroughs, and examples live in code docs and `docs/cookbook.md`.

## Scope

- This spec freezes what scripts and automation can rely on.
- This spec does not freeze internal command wiring, help text prose, or implementation structure.

## Versioning + Compatibility

- The CLI surface follows the package major version: `1.x`.
- Within 1.x, compatibility is additive-only, except for the one-time 1.1.0
  correction that removes the remote MCP bridge, which was added in 1.0.0.
- Existing commands, machine-readable flags, and field meanings must not be removed or redefined.
- New commands/flags/fields may be added when existing behavior remains stable.
- Any other breaking change requires a new major version. Plasmite 1.0 removes
  superseded secure connection commands and options and changes default output
  and history selection. See
  [the upgrade guide](../../docs/record/upgrading-1.0.md).

## Stable Surface

### Stable Command Families

- `plasmite pool create`
- `plasmite pool info`
- `plasmite pool list`
- `plasmite pool delete`
- `plasmite feed`
- `plasmite fetch`
- `plasmite follow`
- `plasmite version`

`duplex`, `tap`, `doctor`, `serve`, `access`, `mcp`, and `completion` also
form the current command inventory. The separately versioned MCP contract
owns its protocol guarantees.

### Machine-Readable Interfaces

- Global `--dir` selects the local pool directory before or after a command.
  It also applies to local MCP stdio. Conflicting repeated directory values
  are rejected. Options after `tap`'s `--` belong to the child process.
- Commands that expose `--json` provide stable machine-readable output through it.
- Output defaults to readable text even when piped. Explicit `--json` selects
  machine output and structured errors, without ANSI color or commentary.
- JSON reports emit one document; JSON streams and append receipts emit one
  document per line. `fetch --json` emits one message envelope.
- `fetch POOL SEQ` defaults to readable text. `--format pretty`, `--format
  json`, and `--format lite3` select readable text, one JSON message envelope,
  and the raw Lite3 document. `--json` selects the same mode as
  `--format json`; combining it with explicit `--format pretty` or
  `--format lite3` is a usage error. Local and remote fetch support all three
  formats.
- `feed --in lite3` reads one byte buffer as one message from `--file PATH`,
  `--file -`, or piped stdin. Stdin input ends at EOF. The document may not
  exceed 256 MiB. Lite3 mode rejects inline `DATA`, `--tag`, and
  `--errors skip`. It preserves the input document bytes, including its
  `meta.tags` string array and object `data`; the append assigns a new
  sequence and time. `feed --json` selects the append receipt format and does
  not change the input mode. Local and remote feed support Lite3 mode.
- Lite3 input treats the whole input as one byte buffer. It preserves unused
  bytes in that buffer and does not split concatenated messages.
- `follow` and `duplex` do not support Lite3 binary streams.
- `version --json` emits its version report; bare `version` emits human text.
- All product-version output uses the same build identity. A clean checkout at
  the matching release tag reports the package version. Other source builds
  append `-dev` and Git commit metadata, plus `.dirty` for changes to tracked
  files. Stage new files to include them in that check.
- Streaming reads provide stable JSON Lines via `--format jsonl` or `--jsonl`.
- `feed` append receipts include `seq`, `time`, and `meta` (not echoed `data`).

### Secure Sharing

Plasmite 1.1 adds native server startup commands without changing the
live-only `serve status` report or its JSON shape.

Plasmite 1.0 provides native access-key sharing. It keeps the existing spec paths and
`/v0` HTTP route prefix. Local pool use remains credential-free.

- `plasmite serve status` lists every Plasmite server running for the current
  OS user. Top-level `--dir` does not limit discovery. `--json` emits a JSON
  array of server objects with stable fields: `pid` (process ID), `pool_dir`
  (absolute pool directory), `local_url`, and `remote_url`. `remote_url` uses
  positional `SERVER`, `--shared-address`, or a loopback HTTPS bind. It is
  `null` when the client-facing HTTPS address is unknown. A non-loopback
  bind needs a public origin even with a TLS certificate configured. Secure
  serving still has a remote listener. Only servers started with this version
  register; restart older running servers to make them discoverable. Stopped
  servers do not appear in the list. With no running servers, the
  human-readable output says `No Plasmite servers running.`
- `plasmite --dir DIR serve` starts the server for the selected pool directory.
  `--bind` and `--remote-bind` set listener addresses; `--shared-address`
  names the client-facing HTTPS origin while retaining the default HTTPS
  listener `0.0.0.0:9743`. Starting in 1.1, positional `serve SERVER`
  also names the public origin and uses its HTTPS port as the default
  listener port. An origin without an explicit port uses 443. `--remote-bind`
  overrides either default for a proxy or selected interface. The two
  public-origin spellings cannot appear together. `--tls-cert` and
  `--tls-key` supply a server certificate; `--front-cert` identifies
  a TLS proxy's public certificate for access-key pinning.
- `plasmite --dir DIR access invite NAME` creates a directory-wide
  access key and displays it only after the server commits it. The earlier
  `--name NAME` spelling remains accepted; the two forms cannot appear
  together.
- `plasmite access connect SERVER_URL` prompts for the access key without
  echoing it, verifies the server, and saves the connection for the current OS
  user. In an interactive macOS or Windows terminal, it then offers browser
  trust for the verified server certificate. It shows the address, all DNS and
  IP subject alternative names, the certificate's SHA-256 fingerprint, expiry,
  and the OS trust-store scope before asking. Declining or failing this step
  leaves the native connection usable. Scripts never wait for this prompt.
- `plasmite access list` inventories saved destination URLs offline, without
  exposing credentials or probing servers. `--json` emits an array of objects
  with a `destination` string.
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
  to the owner, SYSTEM, and administrators; installed services also grant
  access to their virtual account. Startup rejects unsafe
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

### Installed server lifecycle (1.1)

- `serve install [SERVER]` saves the selected pool directory's serve
  options, registers startup before sign-in, and starts it. Linux and macOS
  run the service under the account that owns the pool. Windows registers an
  automatic Windows Service Control Manager service under the pool-specific
  virtual account `NT SERVICE\net.plasmite.<hash>`. Install, update, and
  uninstall require approval through User Account Control (UAC) from an
  administrator account that owns the pool. Plasmite stores no login
  password. Its executable and service setup live under
  `Program Files\Plasmite\Services\<id>`. The pool owner and service account
  share access to `.plasmite-serve` inside the pool directory for service
  state and retained logs. Saved client credentials remain private to the
  account that saved them.
  The service can list parent-folder names along its pool path so Windows
  can prevent those folders from moving during private-state access. Windows
  pool paths may contain spaces or Unicode.
  Startup requires the pool and its storage to be available at boot; it
  cannot bypass a disk unlock or mount that requires a person. If the server
  uses Tailscale, unattended mode must provide network access before sign-in.
- `serve start`, `stop`, `restart`, `logs`, and `uninstall` target the
  installed setup selected by `--dir`. They do not control a foreground
  server. `stop` leaves startup enabled. `uninstall` stops the service and
  removes startup without deleting pools, access keys, or certificates.
  Windows enables native crash recovery after the installed server becomes
  ready. A Windows restart already queued before `stop` can still occur.
  `install` without new options keeps saved settings, installs the current
  executable, and starts the service. On first install,
  positional `SERVER` sets the default HTTPS listener port. A later URL
  change preserves the saved listener unless `--remote-bind` explicitly
  changes it. An update validates the replacement and preserves the
  previous setup on failure. On Windows, update stops the service before
  replacing its executable and setup; rollback preserves service identity,
  certificates, and keys. Updates retain the installed listener addresses
  unless `--remote-bind` changes them.
- `serve status --all` lists installed stopped or failed setups as well as
  live servers for the current OS user. Like plain status, `--dir` does not
  narrow it. `--all --json` emits an array whose rows have `pool_dir`,
  nullable `pid`, `local_url`, nullable `remote_url`, `managed`,
  `startup`, `state`, nullable `problem`, and nullable `setup`.
  `state` is `running`, `starting`, `stopping`, `stopped`, or `failed` for
  reported setups. A stopped setup's addresses describe saved settings,
  not live listeners. `setup` holds `pool_dir`, executable `program`,
  owning `account`, `home`, and effective `run` options. A foreground
  server has `managed: false` and `setup: null`. Install, start, stop,
  restart, and uninstall with `--json` emit one status object with these
  fields; successful `uninstall` reports `state: "uninstalled"`, `startup: false`, and
  `setup: null`. `serve logs --json` streams JSON Lines with one
  `message` field per installed server log line.
- Local `mcp` exposes local pools over stdio. Shared pools use direct HTTPS
  MCP with harness OAuth. MCP methods and messages follow the separately
  versioned [MCP contract](../mcp/2025-11-25/SPEC.md).

## Data + Error Contract

### Message Envelope

- Stable message envelope fields: `seq`, `time`, `meta`, `data`.
- `seq` is monotonic per pool.
- `time` is RFC 3339 UTC text in CLI JSON output.
- `meta.tags` is always present (empty array when unset).
- Message data is JSON. Explicit JSON output preserves the envelope contract.

### Error + Exit Contract

- Errors are emitted on stderr.
- Default stderr is concise human text plus actionable guidance.
- Explicit JSON selects an envelope with required `error.kind` and `error.message`.
- Optional error fields may include `error.hint`, `error.path`, `error.seq`, `error.offset`, `error.causes`.
- Exit-code mapping by error kind is stable for 1.x and defined by implementation in `src/core/error.rs`.

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

### Append Retries

- `feed --retry N` retries `Busy` at most N times after the initial attempt,
  for local and remote pools.
- Append retries do not repeat I/O failures. A flush or connection failure can
  occur after the message becomes visible. Check the pool before resending.

### History and Time Filters

- Bare `follow` starts with new messages. `--tail N` selects the newest N
  retained messages before applying tag or jq filters.
- `--since` accepts RFC 3339 timestamps and relative values such as `5m`,
  resolved at invocation start. Local and remote follow/duplex accept it.
- `follow --no-follow` requires a history selector and stops at the initial
  history boundary even while writers append. `--one` stops at the first match.
- Local `--replay SPEED` requires a history selector and implies finite
  playback. Output deadlines also bound replay waits. Remote replay is rejected.
- CLI `--timeout` is an idle output deadline; API tails retain their separately
  documented absolute timeout.
- Timestamps before the Unix epoch use zero as their comparison time.

### Platforms

- Supported native CLI platforms include macOS and Linux.
- Current official CLI delivery also includes Windows x86_64 through npm and
  PyPI. The current distribution matrix is maintained in
  `docs/record/distribution.md`.

## Non-Contract Surface

The following details are outside the stable machine contract:

- Local `plasmite mcp` uses local pools over stdio. Shared servers use direct
  HTTPS MCP with harness OAuth. MCP methods and messages follow the separately
  versioned [MCP contract](../mcp/2025-11-25/SPEC.md).
- Remote shorthand refs in CLI commands
- Notice payload details and frequency controls

Current remote shorthand constraints (documented, non-frozen):

- URL refs are explicit remote opt-in in core commands that accept pool refs.
- `tap` currently accepts local pool refs only; URL refs are rejected with an actionable usage hint.
- `duplex` remote refs reject `--create`.
- `follow` remote refs reject `--replay`.
- `fetch` and `pool info` accept local or remote pool refs. `pool list` accepts
  an optional server URL. Remote results use the same report fields; unknown
  remote modification time is `null`. A missing local pool directory yields an
  empty list. A directory scan failure fails the command; errors opening individual
  pools remain visible as error rows alongside the other pools.

## References

- CLI operating model: `docs/cli.md`
- Remote protocol contract: `spec/remote/v0/SPEC.md`
- Public API contract: `spec/api/v0/SPEC.md`
