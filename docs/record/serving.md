# Serving and Remote Access

This guide covers native access to pools on another machine. Browser login and
direct remote Model Context Protocol (MCP) authorization are later work.

For the wire-level contract, see `spec/remote/v0/SPEC.md`. The design
proposal remains available at `docs/proposals/serve-mcp-native.md`.

## Start the server

Start the server for a pool directory and give it the HTTPS origin clients will
use:

```console
plasmite --dir ./shared serve --shared-address https://pools.example.net:9743
```

The local HTTP listener binds to `127.0.0.1:9700` and permits credential-free
local pool operations. The remote HTTPS listener binds to `0.0.0.0:9743`.
Both ports can be configured. The shared address is a full HTTPS origin and
must match an address clients can reach.
Secure serving currently runs on macOS and Linux. The Windows client can
connect, but Windows serving waits for protected server-state permissions.

Plasmite retains the TLS identity used for native connections. Clients pin its
public key through the access key. You can supply a TLS certificate and key
with `--tls-cert` and `--tls-key`. Use `--front-cert` when a TLS proxy presents
a different certificate to clients; the access key then pins that front key.
Configure the proxy to verify the backend HTTPS certificate.

## Invite a client

In another terminal on the server machine, create an access key:

```console
plasmite --dir ./shared access invite --name Alex
```

The server must be running for this command. Send the recipient the
HTTPS origin and access key separately. Keep the key private.

## Connect from another machine

On the recipient's machine, connect to the shared origin:

```console
plasmite access connect https://pools.example.net:9743
```

The CLI prompts for the key without echoing it. It verifies the server
certificate against the key before sending the access secret, then saves the
connection for the current OS user. CLI and API pool operations select saved
credentials by destination.

Check a saved connection with:

```console
plasmite access status https://pools.example.net:9743
```

After connecting, use the HTTPS pool URL with supported remote commands:

```console
plasmite follow https://pools.example.net:9743/events
plasmite feed https://pools.example.net:9743/events '{"kind":"ready"}'
```

Remote operations require HTTPS and a saved access key. Do not put the access
key in a URL or command argument.

## Removed access paths

This major version removes the prior public paths: `serve init` and
`serve check`; server `--token` and `--token-file`; client `--token`,
`--token-file`, `--tls-ca`, and `--tls-skip-verify`; remote plaintext and TLS
verification-bypass options; and the old read-only, write-only, and
cross-origin access modes. It provides no compatibility aliases or migration
path. Local pool use remains credential-free.

Browser-based remote access and direct MCP authorization are not part of this
native-sharing release.
