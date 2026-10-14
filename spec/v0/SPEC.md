# Plasmite CLI Spec (1.2 and later)

This document defines the normative CLI compatibility contract starting with Plasmite 1.2.
The historical `spec/v0/` path is retained for existing documentation links;
it does not preserve the superseded pre-1.0 CLI behavior.
It keeps only script-level guarantees; signatures, walkthroughs, and examples live in code docs and `docs/cookbook.md`.

## Scope

- This spec freezes what scripts and automation can rely on.
- This spec does not freeze internal command wiring, help text prose, or implementation structure.

## Versioning + Compatibility

The 1.2.0 breaking cutover removes superseded format and input switches and
changes feed's default message interpretation. Later releases follow the
package major-version compatibility policy.
No compatibility mode accepts the earlier behavior. The historical document
path stays available. See [upgrading to 1.2.0](../../docs/record/upgrading-1.2.md).
Language APIs, HTTP, MCP, and pool files keep their separately versioned contracts.

## Stable Surface

### Stable Command Families

- `plasmite pool create`
- `plasmite pool info`
- `plasmite pool list`
- `plasmite pool delete`
- `plasmite serve --udp` for optional append-only UDP input (see the
  [UDP input contract](../udp/v0/SPEC.md))
- `plasmite feed`
- `plasmite fetch`
- `plasmite follow`
- `plasmite version`

`duplex`, `tap`, `doctor`, `serve`, `access`, `mcp`, and `completion` also
form the current command inventory. The separately versioned MCP contract
owns its protocol guarantees.

### Messages, input, and output

- Global `--dir` selects one local pool directory in every command scope.
  Conflicting repeated values fail. Options after tap's `--` belong to its child.
- Global `--format pretty|json|yaml|lite3|lite3-raw` selects output.
  The default is pretty when stdout is a terminal and compact JSON otherwise.
  JSON reports contain one document; streams contain one object per line.
  YAML uses the same values and marks each document with `---` and `...`.
- `feed`, `fetch`, `follow`, and duplex receive output carry complete messages:
  `seq`, `time`, `meta.tags`, and object `data`. feed writes its complete newly
  appended message. Diagnostics go to stderr and never mix with stdout messages.
- feed accepts a positional object/message, a file, or stdin. JSON/YAML objects
  are application data unless their keys are exactly seq, time, meta, data and
  meta is an object with exactly tags. This reserved complete-message shape
  requires unsigned integer seq, RFC 3339 time, string-list meta.tags, and object
  data. Invalid reserved messages fail; partial envelopes and extra fields are
  application data. Copies preserve data/tags and assign destination seq/time.
  `feed --data` forces application contents. `--tag` is accepted for application
  data and rejected for complete messages. Input source never changes this rule.
- Default feed input recognizes JSON objects, marked YAML documents and framed
  Lite3 from their syntax. It does not wait for another message or EOF after a
  complete record. Explicit --input-format accepts auto, json, yaml, lite3,
  lite3-raw. JSON accepts whitespace-separated objects, including multiline jq
  output. YAML accepts marked documents, or one unmarked document through EOF
  with explicit yaml input. Custom tags, aliases, non-string keys, duplicate
  keys and non-finite numbers fail; ordinary JSON-compatible YAML values retain
  their types.
- The framed Lite3 stream starts with ASCII PLS1 once. Each record matches the
  remote binary header: [u64be seq][u64be timestamp_ns][u32be length][payload].
  fetch produces one record; follow produces many; feed imports each original
  payload without re-encoding. An empty stream may contain zero bytes.
- lite3-raw output requires fetch. Raw input requires --input-format lite3-raw
  and treats the entire file/stdin through EOF as one attachment. Neither binary
  input form accepts --data, --tag or an inline argument. Payloads may occupy at
  most 256 MiB and must fit the destination pool. Text buffering permits JSON
  escaping overhead (six times the payload limit plus 64 KiB). Remote servers
  keep their configured request-body limits.
- All malformed/truncated input stops with a nonzero exit; earlier appends remain.
  Empty streams succeed. Message output uses checked writes and flushes each
  record. A stdout BrokenPipe stops normally without further appends or panic. Other output
  failures return nonzero; stdout failure never retries a successful append.
- Finite history reads return nonzero when retention removes selected messages.
  Live follow reports loss on stderr and continues. access status prints its
  report and returns nonzero when reachability or acceptance fails.
- --json, --yaml, --jsonl, --in, --data-only and --errors are removed.
- Binary output requires a message command. Reports retain their JSON/YAML
  fields. Help/completion remain text, tap preserves child output, MCP keeps
  JSON-RPC. Product version output uses the shared development/release identity.

### Secure Sharing

Plasmite 1.1 adds native server startup commands without changing the
live-only `serve status` report or its JSON shape.

Plasmite 1.0 provides native access-key sharing. It keeps the existing spec paths and
`/v0` HTTP route prefix. Local pool use remains credential-free.

- `plasmite serve status` lists every Plasmite server running for the current
  OS user. Top-level `--dir` does not limit discovery. `--format json` emits a JSON
  array of server objects with stable fields: `pid` (process ID), `pool_dir`
  (absolute pool directory), `local_url`, and `remote_url`. `--format yaml` emits the
  same array as one marked YAML document. `remote_url` uses
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
- `serve --udp` enables one UDP input listener alongside HTTP and HTTPS. Bare
  `--udp` binds `127.0.0.1:9701`; an explicit numeric IP and port uses
  `--udp=ADDRESS`. A space-separated address is not accepted. The listener
  handles all existing pools in the directory selected by global `--dir` and
  needs no public API, endpoint, or status shape change. It adds one message
  per accepted datagram under the separately versioned
  [UDP input contract](../udp/v0/SPEC.md).
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
  exposing credentials or probing servers. `--format json` emits an array of objects
  with a `destination` string; `--format yaml` emits the same array as a marked YAML
  document.
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
- YAML output is available on the same `serve` and `access` commands that
  expose `--format json`; it keeps their report fields and values and marks each
  document with `---` and `...`.
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
  The optional `--udp[=ADDRESS]` setting is saved with the other listener
  settings and survives restarts and executable updates. Omitting `--udp` on an
  update keeps its saved value. `serve install --no-udp` clears it; this action
  conflicts with `--udp` and is available only on `install`. Older setups with
  no UDP setting keep UDP disabled.
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
  narrow it. `--all --format json` emits an array whose rows have `pool_dir`,
  nullable `pid`, `local_url`, nullable `remote_url`, `managed`,
  `startup`, `state`, nullable `problem`, and nullable `setup`.
  `state` is `running`, `starting`, `stopping`, `stopped`, or `failed` for
  reported setups. A stopped setup's addresses describe saved settings,
  not live listeners. `setup` holds `pool_dir`, executable `program`,
  owning `account`, `home`, and effective `run` options. A foreground
  server has `managed: false` and `setup: null`. Install, start, stop,
  restart, and uninstall with `--format json` emit one status object with these
  fields; `--format yaml` emits the same status object as a marked YAML document.
  Successful `uninstall` reports `state: "uninstalled"`, `startup: false`, and
  `setup: null`. `serve logs --format json` streams JSON Lines with one
  `message` field per installed server log line. `serve logs --format yaml` emits
  one marked YAML document with the same field for each log line.
- Local `mcp` exposes local pools over stdio. Shared pools use direct HTTPS
  MCP with harness OAuth. MCP methods and messages follow the separately
  versioned [MCP contract](../mcp/2025-11-25/SPEC.md).

## Data + Error Contract

### Message Envelope

- Stable message envelope fields: `seq`, `time`, `meta`, `data`.
- `seq` is monotonic per pool.
- `time` is RFC 3339 UTC text in CLI structured output.
- `meta.tags` is always present (empty array when unset).
- Message data is JSON. Explicit JSON and YAML output preserve the same
  envelope contract; YAML changes only the serialization format.

### Error + Exit Contract

- Errors and notices are emitted on stderr.
- Default stderr is concise human text plus actionable guidance.
- Explicit JSON selects an envelope with required `error.kind` and `error.message`.
  Explicit YAML selects the same envelope fields as a marked YAML document.
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

### Pool Creation

Local `feed` and `tap` treat `--create-size SIZE` as create-if-missing. Existing
pools remain unchanged; commands without creation options still fail for a missing
pool. Remote feed still rejects creation options, and tap remains local-only.

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
- `follow --no-follow` alone selects all currently retained history; with a
  history selector it reads that selection. It stops at the initial upper sequence
  boundary even while writers append. `--one` stops at the first match.
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

## UDP message targets

`feed`, `follow`, and `duplex` also accept explicit `udp://HOST:PORT/POOL` targets.
`serve --udp-follow` enables UDP reads and implies the default/configured UDP input
address. Existing `--udp` remains append-only. Installed removal/preservation
rules follow the [UDP v1 contract](../udp/v1/SPEC.md).

UDP feed never emits a stored-message receipt. Exit 0 means local socket
submission, not storage. UDP follow/history use the normal filters and formats
with best-effort delivery. Delivery gaps produce notices and finite exit 1;
confirmed retained-history gaps exit 9; no-matching-output timeout exits 124.
No automatic write retry, HTTP fallback, or gap repair. Unsupported commands or
options reject a UDP target before work, with Usage.
