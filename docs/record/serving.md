# Share pools across machines

Plasmite shares a directory of pools over HTTPS. The owner runs the server
and gives each recipient an address and an access key. Recipients can use
the command line, a browser, or a Model Context Protocol (MCP) client such as
Claude Code or Codex CLI.

**An access key grants full access to the shared directory:** listing,
reading, appending, creating, and deleting pools, including pools created
later. Give keys only to people or clients you trust with that access. To
share a smaller set of pools, serve a separate directory.

- [Share your first pool](#share-your-first-pool)
- [Open pools in a browser](#open-pools-in-a-browser)
- [Connect an AI client](#connect-an-ai-client)
- [Manage access](#manage-access)
- [Deploy a server](#deploy-a-server)
- [Troubleshoot a connection](#troubleshoot-a-connection)

## Share your first pool

These instructions cover the upcoming access-key release. Until it ships,
[build Plasmite from source](../building.md#install-the-cli-from-source).
You need it on the server and recipient machines; `plasmite access --help`
checks that your installation includes this workflow. The commands below
work in macOS and Linux shells and Windows PowerShell.

Choose a hostname or IP address the recipient can reach. Replace
`pools.example.net` in every command with that address. Set up DNS if you
use a hostname, and allow recipients to reach TCP port `9743` through your
network and firewall. Keep the local administration port `9700` private.

### 1. Owner: create a pool and start the server

```console
plasmite --dir ./shared pool create events
plasmite --dir ./shared serve --shared-address https://pools.example.net:9743
```

Keep this terminal running. Plasmite creates and retains its server
certificate and private access state. Native clients verify that certificate
using the access key; you can complete this walkthrough with Plasmite's
generated certificate.

### 2. Owner: invite the recipient

Open another terminal on the server machine, in the same working directory:

```console
plasmite --dir ./shared access invite --name Alex
```

The command displays the key. Send Alex the HTTPS address and key through a
private channel. Treat the key as a password: keep it out of command
arguments, URLs, screenshots, and logs. Give each recipient a named key so
you can revoke their access later.

### 3. Recipient: save the connection

```console
plasmite access connect https://pools.example.net:9743
```

Paste the key at the hidden prompt. Plasmite checks the server's name,
certificate validity, and public key before sending the access secret. It
saves the connection for your current OS account.

On macOS and Windows, an interactive terminal also offers browser trust
setup. You can decline that step and use the native connection. For browser
access, follow [the browser setup steps](#open-pools-in-a-browser).

### 4. Recipient: send and read a message

```console
echo '{"text":"hello from Alex"}' | plasmite feed https://pools.example.net:9743/events
plasmite follow https://pools.example.net:9743/events --tail 1
```

The follow command shows the message, then waits for more. Press Ctrl+C to
stop it. The owner can read the same message locally with
`plasmite --dir ./shared follow events --tail 1`.

You now have a saved connection. Later `feed`, `follow`, and `duplex`
commands use it when you name a pool at this server's HTTPS address.

## Open pools in a browser

The browser needs to trust the server certificate before it can show the
login page. Choose the setup that matches your server:

| Server certificate | Recipient setup |
| --- | --- |
| A certificate the browser already trusts | Open the shared HTTPS address and enter the access key. No Plasmite installation needed. |
| A Plasmite-generated certificate on macOS or Windows | Run `access connect` in an interactive terminal, then approve its optional browser trust setup. |
| A Plasmite-generated certificate on Linux | Ask the owner to [serve with a trusted certificate](#use-a-trusted-certificate). Plasmite has no Linux browser trust adapter. |

For Plasmite's browser trust setup:

1. Run `plasmite access connect https://pools.example.net:9743` and enter
   the key. You can run it again if you previously declined browser setup.
2. Review the address, certificate fingerprint, names, expiry, and trust
   scope that Plasmite displays. Answer `y` to the browser trust prompt.
3. Approve the operating system's certificate prompt. On Windows, use a
   signed-in interactive terminal so Windows can show the prompt.
4. Plasmite opens the shared HTTPS address. Enter the access key on that
   page, open `events`, and use Feed to send a message.

The browser keeps a private session for later visits. Sign out to end that
browser session. The key remains usable until the owner revokes it.

### What browser trust changes

On macOS, Plasmite installs the exact server certificate in your login
keychain for SSL. On Windows, it installs the certificate in your account's
Root store. Chrome and Safari on macOS, and Chrome and Edge on Windows,
support this setup. Other applications that use those OS stores may also
use the trust. Browser or device policies can still reject a certificate.

The native connection uses the server's public key; browser setup trusts
the exact certificate. A renewed certificate needs fresh browser approval
even when it keeps the same public key.

Use `access status` to see the current certificate's full SHA-256
fingerprint, then remove that exact trust entry when you no longer need it:

```console
plasmite access status https://pools.example.net:9743
plasmite access untrust CERTIFICATE_SHA256
```

Replace `CERTIFICATE_SHA256` with the full fingerprint. Keep that fingerprint
if you want to remove trust after the certificate changes. Removal also works
when the server is offline. Windows requires its visible approval dialog;
macOS may ask for approval. Native credentials remain saved. Close existing
server tabs and open a fresh browser session to check removal; existing
connections may retain their earlier TLS state. Reopen the browser if it
still uses cached trust. Keep certificate verification enabled.

## Connect an AI client

Both MCP connection methods expose the same pool tools and use the same
access-key authority. Choose according to where you installed Plasmite:

| Method | What the recipient needs | Server certificate trust |
| --- | --- | --- |
| Local MCP process over standard input and output (stdio) | Plasmite and a saved native connection | Plasmite verifies the public key through the access key. |
| Direct HTTPS MCP | Claude Code or Codex CLI; no local Plasmite needed | Both the AI client and its authorization browser must trust the certificate. |

### Use a saved native connection

Run `access connect` first. It prints setup commands for Claude Code and
Codex CLI using your installed Plasmite path. Run the command for your client.
The configured process runs:

```console
plasmite mcp --remote https://pools.example.net:9743
```

Restart Claude Code after adding the server. Ask the client to list pools
and read `events` to check the connection. The client configuration stores
the executable path and address. Plasmite reads the saved credential before
each tool call, so a disconnect or key revocation affects the next call.

### Connect directly over HTTPS

First [deploy a certificate](#use-a-trusted-certificate) that both the AI
client and browser trust. Add the exact `/mcp` address to your client:

```console
claude mcp add --transport http pools https://pools.example.net:9743/mcp
claude mcp login pools
```

For Codex CLI:

```console
codex mcp add pools --url https://pools.example.net:9743/mcp --oauth-client-registration dcr --oauth-resource https://pools.example.net:9743/mcp
codex mcp login pools --oauth-client-registration dcr
```

The login command opens the authorization page. Check the server
address, named client, requested `/mcp` address, and callback. Enter the key
and approve the client. Plasmite issues renewable credentials to that client.
Ask it to list pools and read `events` to confirm access.

Check `claude mcp --help` or `codex mcp --help` if your client's options
differ. Claude Code also offers login through `/mcp` in an interactive session.

An AI client may use a different certificate store from your browser.
Use the local MCP process if it rejects the generated certificate.
Changing the shared address requires fresh direct authorization.

## Manage access

On the server machine, open `http://127.0.0.1:9700/access` to invite clients,
list keys, and revoke access. These controls belong to the local listener.
You can also use the CLI while the server runs:

```console
plasmite --dir ./shared access keys
plasmite --dir ./shared access revoke KEY_ID
```

Replace `KEY_ID` with the ID from the list. Names help you identify clients;
last-used times describe activity this server has observed since startup.
Revocation persists across restarts and ends that key's native, browser,
and MCP access. Other keys keep working.

| Action | Where to do it | Effect |
| --- | --- | --- |
| Sign out | Recipient's browser | Ends that browser session. |
| `access disconnect SERVER_URL` | Recipient's machine | Removes that OS account's saved native connection, including use by local MCP. Works offline. |
| `access untrust CERTIFICATE_SHA256` | Recipient's machine | Removes that exact OS browser trust entry. Keeps the native connection. Works offline. |
| `access revoke KEY_ID` | Owner's machine | Ends access everywhere that uses the key. |

Revocation closes active streams and cancels active MCP waits. Bytes already
sent can still arrive, and a write the server already admitted may finish.
Revoking only an MCP token blocks new calls for that authorization; an
already admitted wait may finish within its 60-second limit.

If you lose a saved native connection, reconnect with the same key. A failed
verification or save preserves the existing credential. If you lose the key,
ask the owner for a replacement and revoke the old key when appropriate.
The server cannot recover the original key from its stored verifier.

If an invitation's response is lost, list the keys, revoke the new entry,
and invite again. If a revocation's response is lost, check the key list
before retrying. Review access after restoring server state from a backup.

## Deploy a server

Secure serving runs on macOS, Linux, and Windows. The shared address names
the HTTPS origin recipients use: scheme, hostname or IP, and port, with no
pool path. It must match the certificate's name and the address your network
routes to the server.

| Listener | Default | Purpose |
| --- | --- | --- |
| Local HTTP | `127.0.0.1:9700` | Pool use and access administration for trusted local processes. No credentials required. |
| Remote HTTPS | `0.0.0.0:9743` | Pool use through access keys, browser sessions, or MCP authorization. |

Use `--bind` and `--remote-bind` to change the listening addresses. Restrict
network access to intended recipients. Keep the local HTTP listener on
loopback and keep it out of any public proxy.

### Use a trusted certificate

Use a DNS name that reaches the server and obtain a certificate from an
issuer your recipients' browsers and AI clients trust. Its Subject
Alternative Name must cover that name. Keep the private key on the server:

```console
plasmite --dir ./shared serve --shared-address https://pools.example.net:9743 --tls-cert /secure/path/fullchain.pem --tls-key /secure/path/privkey.pem
```

Replace the certificate paths with your own; Windows accepts Windows paths.
From a recipient machine, visit the HTTPS address and check that the browser
accepts the certificate. You can also check the server and MCP discovery.
On macOS or Linux:

```console
curl --fail --show-error https://pools.example.net:9743/healthz
curl --fail --show-error https://pools.example.net:9743/.well-known/oauth-protected-resource/mcp
```

In Windows PowerShell:

```powershell
curl.exe --fail --show-error https://pools.example.net:9743/healthz
curl.exe --fail --show-error https://pools.example.net:9743/.well-known/oauth-protected-resource/mcp
```

Both requests must succeed with certificate verification enabled.

If you want recipients to install trust for an owner-supplied certificate,
it must be a restricted server certificate: critical Basic Constraints
`CA:false`, digital-signature use without `keyCertSign`, server-authentication
use, and a DNS or IP name. A certificate that the browser already trusts
needs no Plasmite trust setup.

If an HTTPS proxy presents a different certificate, pass its certificate
to Plasmite with `--front-cert`. New access keys then identify that public
key. Configure the proxy to verify Plasmite's backend HTTPS certificate,
and forward the remote listener only.

### Protect server and client state

Plasmite keeps its identity, key records, browser sessions, and MCP grants
in `.plasmite-serve` inside the pool directory. Protect that directory and
its backups. Restoring older state can restore access you revoked after
the backup.

On Windows, use a filesystem with access control lists, such as NTFS.
Plasmite restricts private state to your account, SYSTEM, and administrators.
It rejects unsafe existing permissions, hard links, junctions, and other
reparse points. It keeps the state directory and its ancestors open while
the server runs to prevent path replacement.

Windows saves client connections under `%APPDATA%\Plasmite` and encrypts
the credentials for your account. `PLASMITE_ACCESS_HOME` selects another
saved-connection directory. Plasmite checks private state ownership and
permissions; inspect an unsafe directory before changing permissions. If
you cannot establish its integrity, use a fresh private directory and
connect again.

### Change the certificate or address

Renew owner-supplied certificates before expiry and restart the server to
load the new files. Plasmite retains its generated identity between starts;
it also upgrades older generated certificates to include `CA:false` while
keeping their public key.

| Change | Recipient action |
| --- | --- |
| New certificate with the same public key | Native keys keep working. Approve the new exact certificate if you used Plasmite's browser trust setup. |
| Different public key | Owner issues new access keys; native recipients connect again. Existing permission records remain until the owner revokes them. |
| Different shared HTTPS address | Connect explicitly to the new address. The same native key works only if the public key still matches and the server accepts it. Authorize direct MCP again. |

Revoke old keys when you intend to end their access. Changing a certificate
does not itself revoke linked browser sessions or MCP grants.

## Troubleshoot a connection

Start with:

```console
plasmite access status https://pools.example.net:9743
```

The output separates three questions: did this OS account save credentials,
can it reach the server, and does the server accept its key? On macOS and
Windows it also reports the current certificate's browser trust state and
fingerprint. On Windows, installed means the exact certificate exists in
your Root store; browser or device policy can still reject it.

| Symptom | What to check or do |
| --- | --- |
| Server unreachable | Check the hostname, port, running server, and network/firewall route from the recipient machine. |
| No saved credentials | Run `access connect` as the OS account that runs the CLI or local MCP process. |
| Key rejected | Ask the owner to check `access keys` for revocation and confirm the key belongs to this server. |
| Certificate name, expiry, or public-key mismatch | Check the exact shared address and ask the owner to correct the certificate or issue a key for its new public key. Keep verification enabled. |
| Native access works; browser rejects the certificate | Reconnect in an interactive terminal and complete browser trust setup, or use a certificate the browser already trusts. Check expiry and device policy. |
| Browser works; direct MCP fails before login | The AI client also needs certificate trust. Use a trusted server certificate or the local MCP process. |
| Browser setup fails or you decline its prompt | The saved native connection remains usable. Retry in a signed-in interactive terminal when you want browser access. |
| Windows reports unsafe state permissions | Use NTFS and inspect the named state directory. Keep Plasmite's private ownership and permissions. |

## Browse pools

On the server machine, open `http://127.0.0.1:9700/ui` for the pool list
or `http://127.0.0.1:9700/ui/map` for the map. Recipients open the shared
HTTPS address after [signing in](#open-pools-in-a-browser).
Serve the HTML through Plasmite; opening files from `ui/` as local pages
cannot reach its pool API.

The map draws each pool's message buffer as a spiral. Byte 0 starts at 12
o'clock on the outside. Each stored message takes its real share of the track;
new messages wrap to the outside when they reach the end. Older messages are
dimmer, the newest is near white, and dark track is unused space. The number
beside the newest message, such as `#505`, is its sequence number, written in
full. Larger buffers get more turns, up to the map's width limit. When a pool
has more than 4,096 messages, the map draws its used space as a continuous
track instead of individual segments.
Click a pool to follow its newest messages; the card above them gives the
pool's message count and capacity. Hover a message to find it on the spiral.
The link at the bottom of that card opens the full pool page.

The pool page shows the pool's last 400 messages, one line each, and adds new
ones as they arrive. Each line has the message's sequence number, time, and
tags, then its data, field by field in the server's order; nested fields show
their dotted path, like `run.id`. A value too long for the line shows its
start and an ellipsis; a long string also shows its size. Tags such as
`error` and `warn` color a line; no field inside the data does. Click a line
to open the whole message below the list, with nested fields indented under
their keys, and a `plasmite fetch` command that reads the same message from a
terminal. There a string longer than 2,000 characters shows its start and
size, with buttons to show all of it or copy it. Click a field's key there to
pin it: every line then shows that field first. This browser remembers pins
for each pool. Load earlier, above the oldest line, reads the 400 messages
before it, as far back as the ring holds.
Type in Filter to keep lines that contain that text, as `path: value` or in a
tag, and mark it; a match in a cut-off part marks the ellipsis. `j` and `k`, or
the arrow keys, move between lines, `/` focuses the filter, and Esc closes the
open message; a hint beside the filter says so. The
Feed box takes JSON, as `plasmite feed` does, or plain text, which it sends
as `{"text": ...}`.

Each open message has an address, such as `/ui/pools/alerts#592`, that opens
the page on it, even when it is older than the messages shown; Copy link copies
it. Click a value or tag in the open message to filter by it. Click the time
column's heading to switch between this machine's time zone and UTC. On any
page, `g` opens a jump to a pool by name. The Pools list moves between pools
with `j` and `k` and opens one with Enter. Each of its rows, and the map's card,
copies the `plasmite follow` command for that pool.

On the local map, the line beneath each pool's capacity says what is using it,
such as "1 following, 1 process", and opens its activity card. The map's own
preview of the open pool is left out of that count. The card lists live
browser, HTTP, and MCP requests for that pool, with peer
IP addresses and client headers when available. The server shares these
observations across its local and HTTPS listeners. It does not resolve peer
addresses through DNS or identify a VM from an address.

On macOS and Linux, the card also lists local processes with an open pool
file when `lsof` can inspect them. An open file does not prove that the process
is following or writing. The card omits the server itself, and short operations
can finish between snapshots. If local inspection fails, the card says so.
These observations require no client registration or heartbeat. The activity
details remain on the local listener.

## Reference

Use `plasmite serve --help` and `plasmite access --help` for command options.
See the [CLI guide](../cli.md) for pool URL and output rules, the
[remote protocol](../../spec/remote/v0/SPEC.md) for HTTP behavior, and the
[MCP contract](../../spec/mcp/2025-11-25/SPEC.md) for authorization and tool
messages.

This major version replaces the earlier token-based setup. It removes
`serve init`, `serve check`, token and verification-bypass flags, remote
plaintext, and read-only, write-only, and cross-origin access modes. Use
the access-key workflow when updating older configurations.
