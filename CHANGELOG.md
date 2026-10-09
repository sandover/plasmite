# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

## [1.2.0] - 2026-10-09

### Raw Lite3 messages in the CLI

Export a message's original Lite3 bytes with `fetch --format lite3`, then
append those bytes with `feed --in lite3`:

```bash
plasmite pool create relay
plasmite feed relay '{"kind":"heartbeat"}'
plasmite fetch relay 1 --format lite3 > heartbeat.lite3
plasmite feed relay --in lite3 --file heartbeat.lite3
```

Both commands support local pools and remote pool URLs. Binary feed accepts
a file or stdin through EOF, up to 256 MiB; a remote server may enforce a
lower request-body limit. It preserves the byte buffer, tags, and data. Each
append gets a new destination sequence and timestamp. Binary fetch writes no
headings, color, or trailing newline. `fetch --format json` also selects the
existing JSON envelope output; `--json` keeps its meaning.

### Binary append fixes

- Reject unreadable nested Lite3 data before appending, including invalid
  UTF-8. This uses the existing JSON decoder to check the data and adds work
  to raw Lite3 validation; ordinary JSON writes keep their existing path.
- Build binary append receipts from the input and assigned sequence/time.
  Concurrent writers can no longer overwrite the message between append and
  receipt generation. Rust clients can use `append_lite3_with_receipt` to
  receive the committed message without a second fetch.

[Full changes since 1.1.0](https://github.com/sandover/plasmite/compare/v1.1.0...v1.2.0)

## [1.1.0] - 2026-10-05

Plasmite 1.1 adds support for running pool servers as operating system services on macOS, Linux and Windows. The CLI handles installation, startup and service management. This release also adds server and client resource limits, restricts write retries, and fixes errors in pool ownership, input handling and recovery.

[Install or update Plasmite](https://github.com/sandover/plasmite/blob/v1.1.0/docs/record/distribution.md#install-matrix).

### Service installation and management

`serve install` configures launchd on macOS, systemd on Linux, or a native Windows service. The service starts at boot. Each pool directory has its own setup.

For an existing pool directory:

```console
plasmite --dir ./shared serve install https://pools.example.net:9743
plasmite serve status --all
plasmite --dir ./shared serve logs --follow
```

Replace the example hostname with a reachable server name. Stop any foreground server for the directory before installing. Installation may require administrator approval.

The CLI provides `serve start`, `stop`, `restart`, `logs` and `uninstall`. Status includes installed servers that have stopped or failed. Uninstalling preserves pools, access keys and certificates.

To update a service, run the new executable's `serve install` command against the same directory. With no new options, it retains the saved settings. If an update fails, Plasmite attempts to restore the previous setup. It retains recovery files when repair requires administrator intervention.

Windows services run under a pool-specific virtual account and do not store a user login password. The [service guide](https://github.com/sandover/plasmite/blob/v1.1.0/docs/record/serving.md) covers setup, recovery and certificate renewal.

### CLI changes

`serve` and `access invite` now accept the server address and invitation name as positional arguments:

```console
plasmite --dir ./shared serve https://pools.example.net:9743
plasmite --dir ./shared access invite laptop
```

The earlier `--shared-address` and `access invite --name` forms remain supported. See the [CLI guide](https://github.com/sandover/plasmite/blob/v1.1.0/docs/cli.md) for listener addresses and saved service settings.

### Security and resource limits

- Shared servers limit concurrent HTTPS connections, stalled requests and pending OAuth approvals. Protected pool and MCP requests check authorization before buffering request bodies.
- Node remote clients refuse redirects and require HTTPS when sending bearer tokens outside loopback.
- Rust remote clients limit response and stream-frame sizes and close malformed or truncated streams.
- Access-key administration verifies that the server belongs to the intended pool directory.

### Data integrity and recovery

- Pool deletion waits for active writes. Handles to a deleted or replaced pool cannot append to it.
- An append that encounters a damaged retained sequence returns an error without changing the pool.
- Python rejects integers that would wrap at the native 64-bit boundary. Filters preserve large JSON integers.
- Input readers bound memory use for oversized records. When skipping bad input, they resume at complete record boundaries.
- Restart and service-update fixes preserve ownership locks and recovery state.

### Upgrade notes

**This release includes breaking changes despite the 1.1 version number.** Existing pool files and the C ABI remain compatible.

#### Remote MCP bridge removal

The remote stdio MCP bridge has been removed. `plasmite mcp --remote SERVER_URL` and `plasmite mcp SERVER_URL` no longer work.

Configure Claude Code or Codex CLI to connect directly to the server's HTTPS `/mcp` endpoint and sign in through OAuth. Local `plasmite --dir DIR mcp` remains available. See the [MCP migration instructions](https://github.com/sandover/plasmite/blob/v1.1.0/docs/record/upgrading-1.0.md).

The public Rust constructor `PlasmiteMcpHandler::with_remote_url` has also been removed. Use `with_client` with a `LocalClient` for an embedded local MCP handler. Remote MCP clients should connect to the server's HTTPS endpoint.

#### Write retries and remote clients

`feed --retry` now retries only a busy pool. After an I/O or connection failure, a write may already have succeeded. Check the pool before sending it again.

Node HTTP clients that depend on redirects or send bearer tokens over non-loopback HTTP must update their connection settings. Oversized remote responses now return errors.

### Platform notes

Linux ARM64 and ARMv7 SDK archives remain preview targets. Physical Raspberry Pi installation and reboot checks remain outstanding.

Windows support continues to target x86_64. Boot-service testing includes an ARM64 Windows VM running the x86_64 binary; it does not establish native ARM64 support.

The local HTTP administration listener remains credential-free. It requires a host or container with trusted local users.

[Full changes since 1.0.0](https://github.com/sandover/plasmite/compare/v1.0.0...v1.1.0)

## [1.0.0] - 2026-10-01

Plasmite 1.0 brings a redesigned CLI, named connections for secure sharing, and a safer foundation for concurrent readers and writers. You can inspect local and remote pools with the same commands, read a finite slice of history, and browse messages without losing large sequence numbers.

**This is a breaking CLI and Rust API release. Existing pool files and the C ABI remain compatible.** Read the [upgrade guide](https://github.com/sandover/plasmite/blob/v1.0.0/docs/record/upgrading-1.0.md) before updating scripts or shared servers.

Install or update using the [supported distribution channels](https://github.com/sandover/plasmite/blob/v1.0.0/docs/record/distribution.md#install-matrix).

### Before you upgrade

- **Scripts must request JSON explicitly.** Commands now use readable output even when piped. Add `--json` wherever you parse output. Streaming `--jsonl` and `--format jsonl` remain supported.
- **Remote access must be set up again.** Old token configuration and insecure TLS options are removed, with no automatic conversion. Update shared servers and recipients together, then use `access invite` and `access connect`.
- **Access keys grant full access to a served directory, including future pools.** The old read-only mode has no replacement in 1.0. Separate directories can isolate groups of pools, but do not provide read-only access.
- **Node.js 24 or later is required.**
- **Linux x86_64 prebuilt CLI binaries require glibc 2.39 or later.** On older systems, see the [source-build requirements and instructions](https://github.com/sandover/plasmite/blob/v1.0.0/docs/building.md#install-the-cli-from-source).
- **Rust callers may need source changes.** Remote-client builders and frame lifetimes changed; see the [Rust migration steps](https://github.com/sandover/plasmite/blob/v1.0.0/docs/record/upgrading-1.0.md#update-rust-clients).

### A more consistent CLI

Readable defaults and explicit JSON make interactive use and scripting predictable. Help is organized around tasks, global directory and color options work before or after commands, and errors explain how to recover.

- Use `fetch` and `pool info` with local pools or remote pool URLs. Use `pool list` with a server URL to inspect a saved connection.
- Use `follow --tail N --no-follow` to read a fixed history snapshot and exit. New messages cannot keep the command running.
- `--tail N` now selects the last N retained messages **before** applying filters. Increase N when you need to search a larger history. `--one` returns the first match.
- Use `serve status` to find running servers for your OS user, including their pool directories and addresses.
- Use `access list` to inspect saved destinations offline, without contacting servers or printing keys.

For example:

```console
plasmite follow events --tail 100 --no-follow --json
plasmite pool list https://pools.example.net:9743
plasmite fetch https://pools.example.net:9743/events 42 --json
```

The remote examples require a saved connection. See [upgrading](https://github.com/sandover/plasmite/blob/v1.0.0/docs/record/upgrading-1.0.md) for setup and the complete list of removed options.

### Secure sharing and Tailscale guidance

Named connections verify the server's identity before sending credentials. Local administration stays on loopback HTTP; remote clients use authenticated HTTPS. Invitations, saved connections, revocation, and browser trust have separate commands.

The [serving guide](https://github.com/sandover/plasmite/blob/v1.0.0/docs/record/serving.md) now covers direct Tailscale connections and optional raw TCP forwarding that preserves Plasmite's TLS identity. Startup messages distinguish the advertised address from the actual listener, and connection errors give clearer hostname, port, and certificate guidance.

**Do not forward the unauthenticated local HTTP port.** An HTTPS-terminating proxy also changes the certificate clients see and needs explicit frontend-identity configuration. Tailscale remains optional; no new network service or policy is enabled automatically.

### Correctness and reliability

- Retained Rust message frames now own stable payload snapshots. Later writes can no longer change data a reader already holds.
- Malformed retained frames return corruption errors instead of hanging. Legal ring-buffer wrap padding remains readable, and impossible array lengths are rejected before allocation.
- Go and Python synchronize native-handle operations and close, preventing overlapping reads, writes, and cleanup from using invalid handles.
- Browser views and Node message envelopes preserve the full 64-bit sequence range. Message links, history cursors, maps, and copied envelopes keep exact IDs.
- Python source installs no longer recursively launch their own CLI wrapper while searching for the native executable.
- `tap` bounds captured lines and terminates its child on reader failure. Terminal output escapes control characters in labels.
- One-shot remote requests use a 30-second network deadline. System DNS resolution is not interruptible, and OS scheduling can extend wall-clock completion. Live streams retain their caller and server limits; backpressured HTTP tails release concurrency capacity when their absolute deadline is observed.

### Performance

Reader notifications now happen after committed writes release the writer lock. This substantially reduces reader delays in the measured concurrent-write workloads while preserving publication order.

Same-host, three-run comparisons with 0.8.0 measured **3–14% lower median time per append and 35–66% lower median time per message with multiple writers**. These are workload-specific results, not a claim that every operation is faster: stable snapshots add copying and locking costs, and very small indexed reads are slower. See the [benchmark methodology and results](https://github.com/sandover/plasmite/blob/v1.0.0/docs/performance/1.0-release.md), including the measured read costs and consumer latency.

### Platform support and limits

- Linux ARM64 and ARMv7 GitHub SDK archives remain **preview** targets. Physical Raspberry Pi installation and reboot qualification is still pending; ARMv6 is unsupported.
- Secure native connections and certificate pinning are available in Rust and the CLI. Other bindings retain their documented capabilities; matching package versions do not imply identical remote-access support.
- Tailscale guidance was checked against current documentation and local TLS/TCP-relay tests. A real two-device tailnet deployment has not been verified.

[Full changes since 0.8.0](https://github.com/sandover/plasmite/compare/v0.8.0...v1.0.0)

## [0.8.0] - 2026-07-28

Plasmite 0.8.0 lets consumers choose whether a retention gap should stop a
message stream. Applications that cannot safely miss data can now fail closed
and recover deliberately, while existing consumers keep the same best-effort
behavior.

### Consumers can detect lost messages

Plasmite pools have bounded capacity. When writers outpace a reader, older
messages may be overwritten before the reader reaches them. A tail can now use
the error gap policy to stop with a structured `RetentionGap` error instead of
silently continuing from the oldest retained message.

The error identifies the first missing sequence. Consumers can use that
boundary to alert an operator, rebuild state from another source, or restart
from an explicitly chosen position.

### The policy works across supported clients

Fail-closed gap detection is available to local Rust, C, Python, Node, and Go
tails. Remote JSON tails expose the same choice and preserve the structured
error across the HTTP boundary.

The default remains best-effort continuation, so existing applications retain
their current behavior. Lite3 remote streaming remains best-effort because its
wire format cannot carry a terminal structured error.

### Retention behavior has a shared contract

The public API specification and cookbook now explain bounded retention,
consumer recovery responsibility, and the difference between best-effort and
fail-closed tails.

Cross-language conformance covers stale starting positions, filtered streams,
overwrite after a tail position is established, and default continuation. This
keeps the Rust implementation and every supported binding aligned.

### Supply chain and releases are leaner

- Vendored Lite3 source now has pinned provenance, integrity verification,
  reproducible update tooling, and sanitizer coverage.
- Homebrew updates download only the release artifacts needed by the formula.
- Publish jobs transfer one normalized copy of each package input.
- Releases skip rehearsal when publishing machinery and credentials are
  unchanged while retaining the same fail-closed live checks.

## [0.7.2] - 2026-07-28

### Improved

- Errors and JSON message representations are now consistent across the CLI,
  Rust API, C ABI, HTTP API, and MCP server.
- MCP capability discovery now comes from the same authoritative descriptors
  used to execute tools, preventing advertised and implemented behavior from
  drifting apart.
- The HTTP server now runs blocking pool storage work through a bounded
  executor. When storage capacity is saturated, it returns a structured busy
  response while health checks and MCP traffic remain responsive.
- Server startup accepts pre-bound listeners and explicit shutdown signals,
  enabling race-free ephemeral ports and predictable teardown in integrations.

### Maintenance

- Release builds, packages, delivery checks, and support documentation now
  share one canonical target manifest.
- CLI implementation is organized by command family behind small execution
  boundaries, reducing coupling without introducing a command framework.
- The 172 CLI integration scenarios are split into focused command-family
  suites with a shared race-free server harness.

## [0.7.1] - 2026-07-28

### Improved

- MCP initialization now tells agents that the visible tool list reflects the
  server's effective access mode.
- MCP guidance now distinguishes exact sequence lookup with `plasmite_fetch`
  from recent, time-windowed, and resumable reads with `plasmite_read`.
- MCP output schemas enumerate every stable `error_kind`, allowing clients to
  reason about failures without prior Plasmite knowledge.

### Release maintenance

- npm publication now uses trusted OpenID Connect publishing instead of an
  expiring repository token.
- PyPI delivery checks now use the Python 3.11 wheel target explicitly.

## [0.7.0] - 2026-07-27

Plasmite 0.7.0 makes the system much easier and safer to approach—especially
for coding agents connecting through MCP with no prior Plasmite knowledge.

### Agents can orient themselves

An MCP client now receives a concise explanation of pools, messages, bounded
retention, and the available operations when it connects. Tool discovery
reflects the server's access mode, and tool schemas describe their defaults,
limits, filtering behavior, output, and retry safety.

Agents can start by listing pools, understand whether to read or wait, and
recover from common mistakes without needing the Plasmite CLI or an
operator-written primer.

### Sequence numbers are optional for normal use

Most users no longer need to think about sequence numbers:

- `plasmite_read` returns recent messages by default.
- `plasmite_wait` without `after_seq` starts at the live edge, like `tail -f`.
- Wait time is bounded and configurable.
- Sequence cursors remain available for advanced replay, resumable polling,
  and reliable repeated wait loops.

### Pool creation is safer

Creating a pool that already exists now returns `AlreadyExists` without
truncating the file or losing retained messages. Invalid pool layouts also
fail before leaving a partial file behind.

MCP errors now explain practical recovery choices: use the existing pool,
create a missing one when appropriate, or check the available pool names.

### The CLI is easier to navigate

Root help now provides the product mental model, a first local workflow, the
complete command inventory, output guidance, and durable links to further
documentation.

Command help now exposes important behavior before failure, including input
selection, local-versus-remote support, required option relationships, server
safety limits, and special exit statuses. A focused CLI guide owns the shared
operating model while the cookbook remains the home for recipes.

### Contract corrections

- MCP rejects unknown arguments instead of silently ignoring them.
- MCP tag filters clearly use all-tags matching; empty tags are rejected.
- MCP no longer advertises jq `where` filtering, which is not implemented on
  that surface.
- `serve init` rejects parent serve options it would otherwise ignore.
- `tap` correctly renders its required wrapped command after `--`.
- Version help now matches its terminal-adaptive output.

### Release and packaging reliability

- Release builds now use one consistent platform-artifact path, with stronger
  provenance and version-alignment checks before publication.
- Homebrew parity is verified before a release can complete, and post-release
  smoke coverage checks the real installation paths across package channels.
- Release status reporting is simpler while retaining fail-closed publishing,
  security, and delivery gates.
- Rust TLS dependencies were updated to clear the current security audit.

## [0.6.1] - 2026-03-03

### Changed
- Follow/message decode hot paths now use Lite3 typed field access for scalar/tag extraction instead of avoidable JSON round-trips.
- Benchmark output now labels runtime lanes explicitly (feed, follow, fetch, contention) and includes stable scenario metadata for docs promotion.
- Added `scripts/bench_runtime_lanes.sh` to generate one reproducible benchmark artifact for README updates.

### Performance
- Local benchmark comparisons against `v0.6.0` show clear feed-path wins (append end-to-end median `ms/msg` improved by roughly 15% in the built-in benchmark matrix).
- A focused 1KB/64MB/`Durability::Fast` 5-run follow-up showed higher cross-process `follow` throughput vs `v0.6.0` (about +9%) on the same machine.
- Follow-up regression analysis (`compare_local_benchmarks.sh`) found no threshold-blocking regressions; remaining `get_scan` slow cases are small, scenario-specific candidates below release gate policy.

## [0.6.0] - 2026-03-02

### Added
- Experimental Model Context Protocol (MCP) v1 support via `plasmite mcp` (stdio) and the `/mcp` HTTP adapter.
- `plasmite tap` for process-output capture workflows.
- `just sdk-from-source` to build release-style SDK tarballs from source for C consumers.

### Changed
- `plasmite serve` startup output now surfaces MCP endpoint details for faster operator discovery.
- `plasmite serve init` default artifact names are now more user-friendly:
  - `plasmite-auth-token.txt`
  - `plasmite-tls-cert.pem`
  - `plasmite-tls-key.pem`
- SDK packaging docs now include source-build guidance and improved Linux `pkg-config` static-link metadata.

### Fixed
- Linux TTY integration testing now works reliably with `util-linux script`.
- `mcp --dir` handling is now command-scoped to preserve top-level CLI behavior.
- Additional MCP and remote integration regressions found during audit were corrected and covered by tests.

## [0.5.1] - 2026-02-26

### Fixed
- Release publish flow is now idempotent and release-note extraction is wired to the finalized changelog section.
- Homebrew tap handling in `release-publish` is verification-only and fail-closed for formula/version/checksum alignment.
- Remote tail streams now preserve structured terminal error envelopes after stream start for JSONL and Server-Sent Events (SSE) encodings.
- CLI internal failures that previously surfaced without guidance now include an actionable retry/backtrace hint.

### Changed
- `pool create`, `pool info`, `pool delete`, and `pool list` now route through `LocalClient`/`PoolRef` paths for more consistent local lifecycle behavior.
- CLI/API/remote v0 specs were distilled to contract-focused docs, with docs-of-record and spec indexes aligned to the frozen compatibility surface.
- Planning log history was renamed from `.ergo/events.jsonl` to `.ergo/plans.jsonl`.

## [0.5.0] - 2026-02-24

### Added
- `plasmite duplex` — read and write a pool from one command. TTY mode wraps each line as `{"from": ME, "msg": LINE}` for live chat; non-TTY mode ingests a JSON stream. Supports `--tail`, `--since`, `--timeout`, `--echo-self`, and remote pools.
- Subcommands now show their help text when required arguments are missing.

### Changed
- CI and release pipeline simplified (-636 lines); consolidated workflow topology.
- Vision and architecture docs deepened into governing documents of record.

## [0.4.0] - 2026-02-18

### Added
- SDK-grade typed APIs across Go, Python, and Node bindings (ergonomic round 2).
- Pool directories are auto-created on `create_pool` across all bindings.
- cargo-binstall preview channel for Linux/macOS.

### Changed
- Documentation indexes and binding READMEs audited for API/default/command accuracy.
- Corrected Go module path to `github.com/sandover/plasmite/bindings/go` so downstream `go get github.com/sandover/plasmite/bindings/go/local` resolves from this repo layout.
- Simplified shared internals: CLI dispatch split into `src/command_dispatch.rs`, pool path/info helpers centralized, and serve tail setup deduplicated.
- Simplified binding maintenance: Node error/type-surface mapping centralized with a declaration drift gate, binding tests use reusable setup helpers, and conformance runners share per-language step-dispatch/pool-open helpers.

## [0.3.0] - 2026-02-16

### Added
- Deterministic cookbook smoke coverage in CI hardening lanes.
- Expanded remote and CLI hardening/security negative-test coverage.

### Changed
- Completed CLI naming migration to `feed` / `follow` / `fetch`.
- Go bindings package layout now uses `bindings/go/api` (pure contracts) and `bindings/go/local` (cgo implementation); import paths changed without a compatibility shim.
- README content was rewritten around real-world use cases and performance framing.

## [0.2.0] - 2026-02-15

### Added
- Windows (`x86_64-pc-windows-msvc`) npm and PyPI install support.
- CI coverage and release plumbing for Windows artifact smoke paths.
- Browser CORS allowlist ergonomics and serving guidance improvements.

### Changed
- Release artifacts now include Windows import-library support needed by bindings.
- Documentation was consolidated into canonical docs-of-record and decision docs.

## [0.1.0] - 2026-02-06

### Added
- `plasmite serve` - HTTP/JSON server with TLS + token auth
- `plasmite doctor` - Pool validation and diagnostics
- Language bindings: Go, Python, Node.js (via libplasmite C ABI)
- Public Rust API (`plasmite::api`)
- Remote protocol spec (HTTP/JSON)
- Conformance test suite (cross-language)
- Remote `feed`/`follow` via shorthand URLs
- Inline seq→offset index for fast `get(seq)` lookups
- `follow --where` filtering with jq predicates
- `follow --tag` filtering with exact tag match
- `follow --replay` for timed playback at configurable speeds
- `follow --one`, `follow --timeout`, `follow --data-only` for scripting
- Shell completion (bash/zsh/fish)
- Web UI (zero-build single-page app) at `/ui`
- Binary releases for macOS (arm64/amd64) and Linux (amd64/arm64)

### Changed
- Pool format: added inline index region (requires pool recreation from v0.0.1)
- Improved CLI help text and error messages
- Performance: 600k+ msg/sec append, sub-ms follow latency

## [0.0.1] - 2026-01-30

### Added
- Initial CLI implementation for local pools.
- Homebrew tap instructions and release workflow.
- CI for formatting, clippy, and tests on Linux/macOS.
- Bench suite for local performance baselines.
- Performance baseline documentation.

### Changed
- Stable JSON-first CLI output and help text, with improved UX defaults.
- Pinned metadata for v0.0.1 and clarified CLI-only scope.
