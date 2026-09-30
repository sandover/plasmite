# Serving and Remote Access

This guide covers access to pools on another machine through native clients,
browsers, and Model Context Protocol (MCP) harnesses.

For wire contracts, see `spec/remote/v0/SPEC.md` and
`spec/mcp/2025-11-25/SPEC.md`. The design proposal remains available at
`docs/proposals/serve-mcp-native.md`.

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
Secure serving runs on macOS, Linux, and Windows. On Windows, use a filesystem
that enforces access control lists, such as NTFS. Plasmite creates server state
with access for your Windows account, SYSTEM, and administrators. It rejects
unsafe existing permissions, hard links, and paths through junctions or other
reparse points. It keeps the state directory and its ancestors open while the
server runs to prevent another process from replacing the path.

Plasmite retains the TLS identity used for native connections. Clients pin its
public key through the access key. You can supply a TLS certificate and key
with `--tls-cert` and `--tls-key`. Use `--front-cert` when a TLS proxy presents
a different certificate to clients; the access key then pins that front key.
Configure the proxy to verify the backend HTTPS certificate.

### Deploy with an existing trusted certificate

Use a DNS name that reaches the server. Obtain a certificate whose Subject
Alternative Name covers that name from a certificate issuer trusted by the
recipient's browser and MCP harness. Keep the private key on the server. Open
TCP port 9743 to recipients and start Plasmite with that exact public address:

```console
plasmite --dir ./shared serve \
  --shared-address https://pools.example.net:9743 \
  --tls-cert /secure/path/fullchain.pem \
  --tls-key /secure/path/privkey.pem
```

Visit `https://pools.example.net:9743` from a recipient machine and check that
the browser accepts the certificate before giving anyone an access key. Check
that `https://pools.example.net:9743/.well-known/oauth-protected-resource/mcp`
is reachable from the same machine. A shell check is:

```console
curl --fail --show-error https://pools.example.net:9743/healthz
curl --fail --show-error https://pools.example.net:9743/.well-known/oauth-protected-resource/mcp
```

Both commands must validate the certificate without a bypass flag. The local
Access page remains on
`http://127.0.0.1:9700/access` on the server machine; do not expose that port.

Renew the certificate before it expires and restart the server so it reads the
new files. A renewal that keeps the same public key preserves native access
keys; a changed public key needs new keys and fresh native connections. Browser
trust setup for a Plasmite-generated certificate uses the exact certificate,
so it needs another approval after renewal. Direct MCP authorization binds to
the public HTTPS address; changing that address needs fresh authorization.
Back up the `.plasmite-serve` directory with the pools and protect the backup:
it contains key and session state. Restoring old state can restore revoked
access.

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

Windows saves connections under `%APPDATA%\Plasmite` with the same private
permissions as server state and encrypts the credentials for your account.
Plasmite rejects existing state with unsafe ownership or permissions. Inspect
that state before changing its permissions; use a fresh private directory and
connect again if you cannot establish its integrity. `PLASMITE_ACCESS_HOME`
selects a different saved-connection directory.

Check a saved connection with:

```console
plasmite access status https://pools.example.net:9743
```

If a saved connection is lost, run `access connect` again and enter the same
key. Plasmite verifies the server again before replacing any saved credential;
a failed verification or save leaves the existing connection in place. This
recovery does not repeat a pool operation. If the server address changed, use
the new address explicitly; the same key works only when it matches the pinned
public key and the server still accepts it. Credentials never follow an address
change on their own.

To forget a local credential, even when the server is offline, run:

```console
plasmite access disconnect https://pools.example.net:9743
```

This removes the connection from the current OS user's saved store. It does not
revoke the key at the server; the owner must run `access revoke KEY_ID` to end
that key's server access. Protect backups of the server's `.plasmite-serve`
directory. Restoring an older copy can restore keys that were revoked after
the backup. A client that loses its only key copy must ask the owner to issue a
replacement and revoke the old key if it should no longer work.

After connecting, use the HTTPS pool URL with supported remote commands:

```console
plasmite follow https://pools.example.net:9743/events
plasmite feed https://pools.example.net:9743/events '{"kind":"ready"}'
```

Remote operations require HTTPS and a saved access key. Do not put the access
key in a URL or command argument.

## Open the pools in a browser

Open the shared HTTPS address in a browser that trusts its certificate. Enter
the access key on the page. The browser keeps a private session cookie, so a
later visit does not need the key again. Sign out on that browser to end its
session. Revoking the access key ends every browser session linked to it.
On the server machine, open `http://127.0.0.1:9700/ui` to browse pools or
`http://127.0.0.1:9700/ui/map` to see the pools in the served directory.
The HTML files in `ui/` need a running server; opening them as `file://`
pages cannot reach the pool API.

The map draws each pool's message buffer as a spiral. Byte 0 starts at 12
o'clock on the outside. Each stored message takes its real share of the track;
new messages wrap to the outside when they reach the end. Older messages are
dimmer, and the newest is near white. The number beside the newest message,
such as `#505`, is its sequence number. Larger buffers get more turns, up to
the map's width limit. When a pool has more than 4,096 messages, the map draws
its used space as a continuous track instead of individual segments.
Click a pool to follow its newest messages; hover a message to find it on the
spiral. The link at the bottom of that card opens the full pool page.

On the local map, the small marks beneath each pool's size open its activity
card. It lists live browser, HTTP, and MCP requests for that pool, with peer
IP addresses and client headers when available. The server shares these
observations across its local and HTTPS listeners. It does not resolve peer
addresses through DNS or identify a VM from an address.

On macOS and Linux, the card also lists local processes with an open pool
file when `lsof` can inspect them. An open file does not prove that the process
is following or writing. The card omits the server itself, and short operations
can finish between snapshots. If local inspection fails, the card says so.
These observations require no client registration or heartbeat. The activity
details remain on the local listener.

The local page at `http://127.0.0.1:9700/access` lists keys and offers Invite
and Revoke. It works only on the server machine. The remote page cannot
administer keys. Browser trust setup for a Plasmite-generated certificate is
an optional step for an installed client; it requires operating-system
approval and installs that certificate. The native saved connection does
not depend on browser trust.

On macOS, Plasmite adds the verified leaf to the current user's login
keychain for SSL. An earlier platform check found that Chrome and Safari
accepted the exact certificate and rejected a renewed certificate and a child
certificate signed with the leaf's key. The operator quit and reopened both
browsers between test phases; the check did not establish whether a restart is
required for trust changes. In the current integration, Chrome accepted the
exact leaf and a same-key renewal left native access intact while browser trust
became false. A CA-signed, restricted localhost leaf also worked when installed
as a trusted root for SSL; macOS rejected a child signed by that leaf. The
effect on other macOS TLS clients has not been established.

A Safari product check completed login, pool write, and read after reload
through the installed trust entry. After `access untrust`, a fresh private
Safari window rejected the certificate while saved native access still read
the message. Safari stayed running throughout this check; existing connections
may keep their prior TLS state.

`access untrust` selects the exact certificate by its full SHA-256 fingerprint
even after the server goes offline or changes its certificate. macOS may ask
the user to authorize removal from the login keychain.

Windows browser certificate setup remains disabled until an ordinary signed-in
user completes the product workflow. Use an HTTPS certificate that Chrome and
Edge already trust. The Windows adapter uses the current user's Root store,
which also serves other Windows TLS clients. `access status` reports whether
that exact certificate exists in the store; browser and device policies can
still reject it. Native saved connections do not require this OS trust.

Older Plasmite-generated server certificates renew once on startup to add an
explicit `CA:false` constraint while keeping their public key. Native access
keys keep working, but browsers must approve the new exact certificate.
Owner-supplied certificates do not change on startup; browser trust setup
requires a leaf with critical Basic Constraints `CA:false`, digital-signature
key use without `keyCertSign`, server-authentication extended use, and a DNS or
IP Subject Alternative Name. A certificate already trusted by the browser
needs no Plasmite trust setup.

The product setup passed Chrome and Edge login and pool write/read under the
VM's administrator account. Exact certificate removal passed from its
interactive console and preserved native access. Windows requires a visible
approval for Root-store changes; a headless removal fails. Ordinary-user
browser setup remains unproven. Run `access untrust` from a signed-in
interactive terminal so Windows can show its approval dialog.

## Connect an MCP harness

For a recipient who installed Plasmite, save the native connection first.
Then use `plasmite mcp --remote https://pools.example.net:9743` as a local MCP
server. It reads that OS user's saved connection at tool-call time. The
harness configuration contains the address and command, not the access key.
Disconnecting the saved connection makes later local MCP calls fail until the
recipient connects again.

For a recipient without Plasmite, use a certificate already trusted by both
the harness and browser. Add `https://pools.example.net:9743/mcp` as a remote
HTTP MCP server. For the installed versions of these harnesses, the setup
commands are:

```console
claude mcp add --transport http pools https://pools.example.net:9743/mcp
codex mcp add pools --url https://pools.example.net:9743/mcp \
  --oauth-client-registration dcr \
  --oauth-resource https://pools.example.net:9743/mcp
codex mcp login pools --oauth-client-registration dcr
```

The harness opens the server's authorization page. Check its HTTPS address,
requested pool address, and callback, then enter the access key. Plasmite
issues renewable credentials to that harness. Revoking one harness token ends
that authorization for new calls; a wait already admitted may finish within
its 60-second cap. Revoking the access key ends every linked authorization
and active wait.
A changed public address needs new authorization. The MCP message and OAuth
contracts are in `spec/mcp/2025-11-25/SPEC.md`.

List access keys with `plasmite --dir ./shared access keys`. The list shows
names, opaque IDs, creation times, revocation state, and when this server last
saw each key.
Use `plasmite --dir ./shared access revoke ID` to revoke one. Revocation takes
effect before the command reports success. Existing streams close, and the
other keys keep working. The server retains revocations across restarts.
If an invitation response is lost, list the keys, revoke the new entry, and
create another invitation. Review keys after restoring server state from a
backup.

## Removed access paths

This major version removes the prior public paths: `serve init` and
`serve check`; server `--token` and `--token-file`; client `--token`,
`--token-file`, `--tls-ca`, and `--tls-skip-verify`; remote plaintext and TLS
verification-bypass options; and the old read-only, write-only, and
cross-origin access modes. It provides no compatibility aliases or migration
path. Local pool use remains credential-free.
