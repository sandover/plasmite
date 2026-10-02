# Upgrade to Plasmite 1.0

Plasmite 1.0 replaces the earlier remote token setup with named access keys,
makes CLI output explicit, and gives local and remote readers the same history
rules. Existing pool files and the C ABI remain compatible. Back up your
configuration and keep your pools when replacing the executable.

## Requirements

The Node package requires **Node.js 24 or newer**. The Python, Rust and native
SDK requirements remain in the [distribution guide](distribution.md). Linux
ARM64 and ARMv7 archives remain previews; physical Raspberry Pi installation
and reboot checks remain outstanding. ARMv6 is unsupported.

## Update scripts to request JSON

`feed`, `fetch`, `version` and access commands now produce readable output by
default even when piped. Add `--json` wherever a script parses their output.
Reports emit one document; message streams and append receipts emit one
document per line. Structured output has no color or commentary.

```console
plasmite feed events '{"ready":true}' --json
plasmite fetch events 1 --json
plasmite follow events --tail 100 --no-follow --json
plasmite version --json
plasmite access status https://pools.example.net:9743 --json
```

`--jsonl` and `--format jsonl` keep their streaming meanings. Errors default
to readable stderr; explicit JSON selects the structured error envelope.
`mcp` always uses JSON-RPC and `completion` always prints shell code.

`--dir` and `--color` work before or after the command. One `--dir` applies to
the whole invocation, including `mcp`; conflicting repeated values fail.
Options after `tap`'s `--` belong to the child process.

## Connect MCP clients to shared pools

Plasmite 1.1.0 removes the remote MCP bridge introduced in 1.0.0. The removed
forms are `plasmite mcp --remote SERVER_URL` and `plasmite mcp SERVER_URL`.
This is a one-time correction to the 1.x CLI compatibility promise. Keep using
`plasmite mcp` over stdio for local pools. For a shared server, configure the
MCP harness to connect directly to `https://HOST[:PORT]/mcp` and complete its
OAuth login. For example:

```console
claude mcp add --scope user --transport http plasmite https://pools.example.net:9743/mcp
claude mcp login plasmite
codex mcp add plasmite --url https://pools.example.net:9743/mcp --oauth-client-registration dcr --oauth-resource https://pools.example.net:9743/mcp
codex mcp login plasmite --oauth-client-registration dcr
```

The harness and its authorization browser must trust the server certificate.
The local Plasmite CLI does not need a saved connection for direct MCP access.

## Read history deliberately

`--tail N` counts the last N retained messages before applying `--tag` and
`--where`. Earlier versions counted matching messages. Increase N if you need
to search a larger history. `--one` exits after the first match.

`--no-follow` with `--tail` or `--since` reads a fixed history snapshot and
exits; later appends cannot prolong it. Relative `--since` values use the
same starting point. These rules apply to local and remote pools.
`--replay SPEED` remains local, requires a history selector, and implies a
finite read. A future `--since` without `--no-follow` keeps waiting for new
messages rather than exiting immediately.

## Replace remote token configuration

Old tokens and configuration have no automatic conversion or compatibility
aliases. Update the server and its recipients together. Keep the pool
directory; create new access keys for recipients.

The following commands and options are removed:

- `serve init` and `serve check`.
- Server `--token`, `--token-file`, `--access`, `--tls-self-signed`,
  `--allow-non-loopback`, `--insecure-no-tls` and `--cors-origin`.
- Remote `feed`/`follow` `--token`, `--token-file`, `--tls-ca` and
  `--tls-skip-verify`.

On the owner machine, start a server with local administration on loopback
and authenticated HTTPS for recipients:

```console
plasmite --dir ./shared serve --shared-address https://pools.example.net:9743
```

In a second owner terminal, create a separate key for each recipient:

```console
plasmite --dir ./shared access invite --name laptop
```

Pass the HTTPS address and key through a private channel. On the recipient
machine, enter the key at the hidden prompt:

```console
plasmite access connect https://pools.example.net:9743
plasmite pool list https://pools.example.net:9743
plasmite pool info https://pools.example.net:9743/events
plasmite follow https://pools.example.net:9743/events --tail 10
```

`--bind` now addresses the local loopback listener. Use `--remote-bind` for
the HTTPS listener; its default is `0.0.0.0:9743`. Existing PEM certificates
can still use `--tls-cert` and `--tls-key`. A TLS proxy uses `--front-cert`
for its public identity. See the [serving guide](serving.md) for certificates,
browser approval, MCP setup and network requirements.

An access key grants full access to one served directory, including future
pools. The earlier read-only/access-mode configuration has no equivalent.
Use separate directories and keys where clients need different scope.
`access list` inventories saved destinations; `access status` checks one.
`access disconnect` forgets a local credential; the owner uses `access revoke`
to withdraw permission. Browser trust is separate and uses `access untrust`.

Restart servers with the new executable so `serve status` can discover them.

## Keep Node message sequences exact

`Message.seq` and `Lite3Frame.seq` were already `bigint` in 0.8. Version 1.0
fixes precision loss while parsing numeric sequence tokens from message bytes;
it does not introduce a new sequence result type. User payload numbers retain
ordinary JSON number behavior.

Pass raw message bytes directly to `parseMessage`, rather than parsing them
with ordinary `JSON.parse` first. Manually constructed envelopes must use a
`bigint` or decimal string for sequences above `Number.MAX_SAFE_INTEGER`;
unsafe numeric inputs are rejected instead of silently rounded.

```javascript
const { parseMessage } = require("plasmite");
const message = parseMessage(rawMessageBuffer); // a Buffer from getJson/nextJson
const checkpoint = JSON.stringify({ seq: message.seq.toString() });
const restoredSeq = BigInt(JSON.parse(checkpoint).seq);
console.log(message.raw.toString("utf8")); // serialize the message envelope
```

The checkpoint deliberately stores a decimal string. Do not convert a large
sequence to `Number`, or pass a `bigint` directly to `JSON.stringify`.

## Update Rust clients

The Rust builders `with_token`, `with_tls_ca_file` and `with_tls_skip_verify`
are removed. A client can use the saved connection established by the CLI:

```rust
use plasmite::api::RemoteClient;
let client = RemoteClient::new("https://pools.example.net:9743")?;
```

For an in-memory connection, obtain a full access key through your
application's secret input and use:

```rust
let client = RemoteClient::with_access_key(server_url, access_key)?;
```

Both paths verify the certificate name, validity and public-key fingerprint
before sending credentials. A pool URI must name the client's origin.

Secure native access-key APIs belong to Rust and the CLI. Node's HTTP client
does not load saved native connections or implement access-key certificate
pinning; its loopback HTTP use remains available. Other bindings retain their
documented local APIs. Package versions move together without implying that
every binding exposes every transport feature.

Core `FrameRef` payloads now own their bytes (`Vec<u8>`) and no longer borrow
the pool mapping. `FrameRef` and `CursorResult` no longer take lifetime
parameters. Update annotations and borrow `&frame.payload` when a decoder
expects a slice. Retained frames remain stable after later appends; C ABI
ownership and the on-disk format do not change.

If you use the low-level `Pool::append_lock`, its independent exclusive guard
now remains effective until dropped. Drop it before reading or appending on
the same thread; those operations wait for the guard. Normal append methods
already acquire their own lock. Reopen a pool whose path has been replaced
before requesting an explicit guard.
