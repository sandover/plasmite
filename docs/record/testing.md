# Testing

Run the cheapest check that covers your change while you work, then widen the net before you push, merge and release.

## What to run

| When | Command | What it adds |
| --- | --- | --- |
| While editing | `cargo check`, then `cargo test --lib <name>` or `cargo test --test <target> <name>` | The one module or target that covers the change. |
| Before every push | `just check` | Formatting, Clippy with warnings denied, the default Rust test suite, the benchmark-harness and dev-server script tests, version alignment, release-target checks and Lite3 integrity. Pull requests run the same command. |
| When a change touches bindings, the C ABI, packaging or cookbook examples | `just integration` | The OSC example, cookbook examples, C ABI smoke and misuse checks, all binding suites, the native-library contract smoke, cross-language artifact checks and the browser UI smoke. Needs Go, Node, Python and `uv`. |
| Before merging, tagging or publishing | `just release-gate` | `check` plus `integration`, a release build of the shared library and the Python wheel install smoke. CI runs it on main and on a schedule, on Linux and macOS. |
| Before a release | `just audit` | RustSec advisories, using a database kept under `.scratch/`. `just release-check` runs `release-gate` and `audit` together. |

`just check` runs `cargo clean` first when `target/` has grown past 2 GB.

For local checks, install `just`, `jq`, Python 3 and the Rust and C toolchains from the [build guide](../building.md). Run `npm ci` in `bindings/node` before the integration checks. Install `cargo-audit` before `just audit`, and give it network access to refresh the advisory database.

## Rust test targets

`cargo test --lib` runs the library's unit tests: frame encoding, append planning, pool I/O and locking, cursor overwrite safety, error mapping, Lite3 validation, views and the C ABI. `cargo test --bin plasmite` runs the unit tests for the CLI and server modules.

The integration targets in `tests/` run the real `plasmite` binary or the public Rust API. They share process, terminal, JSON, temporary-pool and server helpers in `tests/support/`. Tests that launch a server can use an OS-assigned port to avoid collisions.

| Target | Covers |
| --- | --- |
| `cli_pool` | Pool lifecycle, metadata, `doctor`, permissions and corrupt files. |
| `cli_feed_fetch` | Local and remote `feed`, input framing, receipts and exact lookup. |
| `cli_follow_duplex` | Local and remote `follow`, filters, positions and cancellation. |
| `cli_duplex` | `duplex` input, independent replies and session end. |
| `cli_history` | Finite history selection and local/remote parity. |
| `cli_pipes` | Complete-message pipes between commands, closed pipes and stream failures. |
| `cli_binary_pipes` | Exact Lite3 payloads through local and HTTP streams. |
| `cli_yaml`, `cli_yaml_stream` | YAML reports and messages, and live YAML delivery. |
| `cli_syntax`, `cli_global_options` | Accepted spellings and the placement of shared options. |
| `cli_help_output` | Help, completions, color, JSON output, error envelopes and exit codes. |
| `cli_tap` | `tap` is rejected as an unknown command. |
| `cli_remote_inspection` | Local and remote pool inspection. |
| `cli_access_output` | `access` output and offline connection discovery. |
| `cli_service`, `cli_service_logs` | Foreground and installed servers, and `serve logs`. Windows upgrade tests are opt-in; see below. |
| `serve_status` | `serve status` discovery, stale entries and private registry files. |
| `secure_serving` | Server validation, limits, TLS, response headers and MCP over stdio. |
| `secure_sharing` | An access key across a server restart and a fresh client. |
| `access_lifecycle` | Key listing, revocation of open streams and persistence. |
| `browser_access` | Browser sessions against the same access records. |
| `oauth_access` | OAuth for the HTTPS MCP endpoint across a restart. |
| `remote_integration` | The HTTP protocol and Rust remote client, including errors and continuation across server limits. |
| `udp_input`, `udp_client`, `udp_transport`, `udp_transport_cli` | UDP input, the client, the full transport through a lossy proxy, and the CLI over UDP. |
| `conformance_v2` | The shared 2.0 read cases through the CLI and the Rust API. |
| `owned_view_streams` | Retained view snapshots across ring overwrite. |
| `lock_smoke` | Several `feed` processes on one pool serialize their writes. |
| `build_version`, `build_version_cargo` | Build identity from Git tags, commits and local changes. |

## Contract checks

`tests/fixtures/conformance/reads.json` defines the 2.0 read cases: starting history, a `since` or `last` selection, optional tags, messages added after the reader opens, and the expected sequences or error kind. `just conformance` runs them through the CLI and the Rust API. The Go, Python and Node suites read the same file. One case opens `tests/fixtures/pool-v1.3.0.plasmite`, a pool written by Plasmite 1.3.0, to prove 2.0 reads the existing format. Copy that fixture to a temporary directory before writing to it.

`tests/fixtures/owned_views.json` states the intended input rules for message views, and `uv run python tests/owned_view_contract.py` checks that the cases agree with each other. No binding suite reads this file yet; each binding tests views through its own cases. Measurements live in the [language-boundary results](../performance/language-boundary-results/owned-views/README.md).

To check MCP compatibility against the maintained Python SDK, build the binary with `cargo build --bin plasmite`, then run `uv run --no-project --with mcp==2.3.0 python scripts/mcp_protocol_smoke.py target/debug/plasmite`. It checks automatic discovery and explicitly selected 2026-07-28 requests against disposable local pools.

## Bindings and the C ABI

Each binding loads the major-versioned native library (`libplasmite.so.2`, `libplasmite.2.dylib` or `plasmite2.dll`). The `just` recipes build the library and stage that name with `scripts/stage_versioned_library.sh`, so run binding tests through them:

```bash
just bindings-go-test
just bindings-python-test
just bindings-node-test      # also packs the npm package and runs the remote-only smoke
just bindings-test           # all three plus the Node type check
just bindings-go-contract-test   # Go API contracts without cgo
```

Each suite covers message data and tags, structured errors, retention gaps, views, streams and its UDP client against a child server. Node's suite also checks that runtime exports match `types.d.ts`, and `npm run test:portable` in `bindings/node` checks the bundled WebAssembly core.

`just cross-artifact-smoke` runs after the bindings and CLI are built: Go creates a pool, Node streams a message written by a separate CLI process, and Node, Python and the CLI read each other's messages from one file.

`just abi-smoke` links a small C program against the library and runs `tests/abi_adversarial.c`, which checks documented failure and cleanup behavior such as null arguments and repeated cleanup. On Linux, `just test-lite3-sanitizers` runs the same harness against an AddressSanitizer and UndefinedBehaviorSanitizer build at `-O0` and `-O2`. The harness does not probe invalid addresses, stale handles, wrong nonzero lengths or double frees: `include/plasmite.h` makes those the caller's obligation, and they are undefined behavior before the library can check them.

## Windows

To run the Windows private-state and secure-sharing checks locally, use PowerShell:

```powershell
cargo test --release --lib windows_private::tests -- --test-threads=1
cargo test --release --bin plasmite serve_service::windows::tests -- --test-threads=1
cargo test --release --test secure_sharing --test access_lifecycle --test browser_access --test oauth_access -- --test-threads=1
```

The service upgrade tests register and remove disposable native services, so they need an administrator terminal:

```powershell
cargo test --test cli_service windows_upgrade:: -- --ignored --test-threads=1
```

Set `PLASMITE_WINDOWS_PREVIOUS_EXE` to an earlier `plasmite.exe` to test an upgrade from that version.

## What automated tests cannot show

Tests can check saved service settings, status output and error recovery, but not that a server starts before anyone logs in after a reboot, or that an archive works on physical Raspberry Pi hardware. Those claims need a dated manual check; [validation reports](../validation/README.md) describe how to run and record one.

## Benchmarks

Benchmarks are not tests. `just bench` runs the core benchmark harness in release mode; [performance](../performance/README.md) explains which report to use.
