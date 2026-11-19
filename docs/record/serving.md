# Share pools across machines

Plasmite shares a directory of pools over HTTPS. The owner runs the server and gives each recipient an address and an access key. Recipients use the command line, a browser, or a Model Context Protocol (MCP) client such as Claude Code or Codex CLI.

## What you expose

- **An access key grants full access to the served directory.** Its holder can list, read, feed, create and delete every pool in it, including pools created later. To share fewer pools, serve a separate directory.
- **The local listener needs no credentials.** It binds `127.0.0.1:9700`, and every OS user who can reach that port can read and change pools and create access keys. Run the server on a host or container whose local users you trust with the whole directory, and never forward this port through a proxy.
- **The remote HTTPS listener binds all IPv4 interfaces by default.** It listens on `0.0.0.0` at the server URL's port (`9743` with no URL, `443` for a URL without a port). Use `--remote-bind` to choose an address, including an IPv6 address.
- **UDP has no authentication or encryption.** `--udp` lets any sender that reaches the socket feed every pool; `--udp-follow` also lets it read them. UDP stays off until you enable it.

## Contents

- [Share your first pool](#share-your-first-pool)
- [Open pools in a browser](#open-pools-in-a-browser)
- [Manage access](#manage-access)
- [Deploy a server](#deploy-a-server)
- [Send and read over UDP](#send-and-read-over-udp)
- [Connect an AI client](#connect-an-ai-client)
- [Troubleshoot a connection](#troubleshoot-a-connection)
- [Reference](#reference)

## Share your first pool

Install Plasmite 2.0 on the server and on each recipient machine. The server rejects the 1.x HTTP pool protocol. The commands below work in macOS and Linux shells and in Windows PowerShell. See [distribution](distribution.md) for installation options.

Choose a hostname or IP address the recipient can reach, and replace `pools.example.net` in every command with it. Set up DNS if you use a hostname, and let recipients reach TCP port `9743` through your network and firewall. Keep the local administration port `9700` private.

### 1. Owner: create a pool and start the server

```console
plasmite --dir ./shared pool create chat
plasmite --dir ./shared serve https://pools.example.net:9743
```

Keep this terminal running. Plasmite creates and keeps its own server certificate and private access state. Native clients verify that certificate through the access key, so this walkthrough works with the generated certificate.

### 2. Owner: invite the recipient

Open another terminal on the server machine, in the same working directory:

```console
plasmite --dir ./shared access invite laptop
```

The command prints the key. Send the recipient the HTTPS address and key through a private channel. Treat the key as a password: keep it out of command arguments, URLs, screenshots and logs. Give each recipient a named key so you can revoke that recipient alone.

### 3. Recipient: save the connection

```console
plasmite access connect https://pools.example.net:9743
```

Paste the key at the hidden prompt. Plasmite checks the server's name, certificate validity and public key before it sends the access secret, then saves the connection for your OS account.

On macOS and Windows, an interactive terminal also offers browser trust setup. You can decline it and keep the native connection; see [Open pools in a browser](#open-pools-in-a-browser).

### 4. Recipient: send and read a message

```console
echo '{"text":"hello from the laptop"}' | plasmite feed https://pools.example.net:9743/chat
plasmite follow https://pools.example.net:9743/chat --last 1
```

`follow` shows the message, then waits for more; press Ctrl+C to stop it. The owner can read the same message locally with `plasmite --dir ./shared follow chat --last 1`.

Every later `feed`, `fetch`, `follow` and `duplex` command that names a pool at this address uses the saved connection.

## Open pools in a browser

The browser must trust the server certificate before it can show the login page. Choose the setup that matches your server:

| Server certificate | Recipient setup |
| --- | --- |
| A certificate the browser already trusts | Open the HTTPS address and enter the access key. No Plasmite installation needed. |
| A Plasmite-generated certificate on macOS or Windows | Run `access connect` in an interactive terminal and approve its browser trust step. |
| A Plasmite-generated certificate on Linux | Ask the owner to [serve with a trusted certificate](#use-a-trusted-certificate). Plasmite cannot install browser trust on Linux. |

For Plasmite's browser trust setup:

1. Run `plasmite access connect https://pools.example.net:9743` and enter the key. Run it again if you declined browser setup earlier.
2. Review the address, certificate fingerprint, names, expiry and trust scope that Plasmite shows. Answer `y` to the browser trust prompt.
3. Approve the operating system's certificate prompt. On Windows, use a signed-in interactive terminal so Windows can show the prompt.
4. Plasmite opens the HTTPS address. Enter the access key, open `chat`, and use Feed to send a message.

The browser keeps a private session for later visits. Sign out to end it; the key stays valid until the owner revokes it.

### What the browser shows

The pool list lists every pool in the served directory. Open one to see its latest messages, one line each, with new messages added as they arrive. Click a line to see the whole message and the `plasmite fetch` command that reads it; filter lines by text or tag; and use Feed to send JSON or plain text. The map draws each pool's ring buffer as a spiral, so you can see how full each pool is and where its newest message sits. If someone replaces a pool you are watching, the page switches to the new pool's history and says so.

On the server machine, open `http://127.0.0.1:9700/ui` for the pool list or `http://127.0.0.1:9700/ui/map` for the map. The local map also shows, for each pool, the live browser, HTTP and MCP readers and, on macOS and Linux, local processes that hold the pool file open.

### What browser trust changes

On macOS, Plasmite installs the exact server certificate in your login keychain for SSL. On Windows, it installs the certificate in your account's Root store. Chrome and Safari on macOS, and Chrome and Edge on Windows, support this setup. Other applications that use those OS stores may also trust the certificate, and browser or device policy can still reject it.

The native connection trusts the server's public key; browser setup trusts the exact certificate. A renewed certificate needs fresh browser approval even when it keeps the same public key.

To remove browser trust, find the certificate's SHA-256 fingerprint with `access status`, then remove that entry:

```console
plasmite access status https://pools.example.net:9743
plasmite access untrust CERTIFICATE_SHA256
```

Keep the fingerprint if you may need to remove trust after the certificate changes. Removal works while the server is offline and leaves native credentials saved. Windows shows an approval dialog; macOS may ask for approval. Close open server tabs, or restart the browser, before you check that removal worked, because open connections can keep their earlier trust.

## Manage access

### Owner: list and revoke keys

On the server machine, open `http://127.0.0.1:9700/access` to invite clients, list keys and revoke access. The same controls work from the command line while the server runs:

```console
plasmite --dir ./shared access keys
plasmite --dir ./shared access revoke KEY_ID
```

Replace `KEY_ID` with the ID from the list. Last-used times cover activity this server has seen since it started. Revocation persists across restarts and ends that key's native, browser and MCP access; other keys keep working. It closes active streams and cancels active MCP waits, though bytes already sent can still arrive and a write the server already accepted may finish.

### Recipient: check and remove saved connections

```console
plasmite access list
plasmite access status https://pools.example.net:9743
plasmite access disconnect https://pools.example.net:9743
```

`access list` shows this OS account's saved servers without contacting them. `access status` checks one server: whether this account saved credentials, whether it can reach the server, and whether the server accepts the key. It exits nonzero when the server is unreachable or rejects the key. `access disconnect` forgets the saved connection, even while the server is offline; it does not revoke the key on the server.

| Action | Where | Effect |
| --- | --- | --- |
| Sign out | Recipient's browser | Ends that browser session. |
| `access disconnect SERVER_URL` | Recipient's machine | Removes this OS account's saved connection. Works offline. |
| `access untrust CERTIFICATE_SHA256` | Recipient's machine | Removes that exact browser trust entry and keeps the native connection. Works offline. |
| `access revoke KEY_ID` | Owner's machine | Ends access everywhere the key works. |

### Recover lost keys and connections

If you lose a saved connection, run `access connect` again with the same key. A failed check or save keeps the existing credential. If you lose the key, ask the owner for a new one; the server stores only a verifier and cannot recover the original. The owner can then revoke the old key.

If an `invite` response gets lost, list the keys, revoke the new entry, and invite again. If a `revoke` response gets lost, check the key list before you retry. Review access after you restore server state from a backup.

## Deploy a server

Secure serving runs on macOS, Linux and Windows.

### Choose listeners and addresses

| Listener | Default | Purpose |
| --- | --- | --- |
| Local HTTP | `127.0.0.1:9700` | Pool use and access administration for trusted local processes. No credentials. |
| Remote HTTPS | `0.0.0.0:9743` when no server URL sets the port | Pool use through access keys, browser sessions or MCP authorization. |
| UDP | Off; `127.0.0.1:9701` when enabled | `--udp` accepts input; `--udp-follow` also serves follow. No credentials. |

The positional `SERVER` URL names the HTTPS origin recipients use: scheme, hostname or IP, and port, with no pool path. It must match the certificate's name and the address your network routes to the server. It also sets the certificate names and the remote listener's port (443 when the URL has no port). Its hostname never selects a bind interface.

`--bind` moves the local listener, which must stay on loopback. `--remote-bind` takes a numeric IP and port (bracket an IPv6 address) and overrides the URL's port, for example to bind one interface or to sit behind a proxy that uses a different backend port.

The HTTPS listener holds at most 128 connections, and these limits apply before any access-key check:

- It closes a TLS handshake that takes longer than 5 seconds, waits at most 10 seconds for HTTP/1 headers, and times out a request body after 30 seconds without data.
- It pings HTTP/2 clients every 15 seconds and closes a connection that fails to answer within 10 seconds. Idle HTTP/2 connections that answer stay open and count toward the limit, so requests above it must retry.
- It ends an HTTP follow whose output queue stays full for 30 seconds. `--max-tail-timeout-ms` changes that duration; the same setting limits how long a read takes to open and how long one fetch response may run.
- OAuth registration and authorization pages allow 30 requests per minute from each TCP source address. One client can hold 8 pending approvals and one source address 32. A reverse proxy shares one source address among its clients.

Put an Internet-facing server behind a proxy or firewall that also limits connection rates.

### Start a server at boot

`serve install` registers and starts a service that the operating system can launch at boot, once the storage and network it needs are available. Linux needs systemd. On macOS Plasmite registers a system LaunchDaemon. Windows uses the Service Control Manager.

```console
plasmite --dir ./shared serve install https://pools.example.net:9743
plasmite serve status --all
```

On Linux and macOS, the service runs as the account that owns the pool directory; Plasmite may ask for administrator approval to register it. On Windows, it runs as an automatic service under a virtual account for that pool, `NT SERVICE\net.plasmite.<hash>`. The administrator who owns the pool approves install, update and uninstall through User Account Control (UAC), and Plasmite stores no login password. Saved client connections stay private to the account that saved them.

Manage the installed service with:

```console
plasmite --dir /srv/shared serve logs
plasmite --dir /srv/shared serve stop
plasmite --dir /srv/shared serve start
plasmite --dir /srv/shared serve restart
plasmite --dir /srv/shared serve uninstall
```

Use an absolute `--dir` when you run these from another working directory. They act on the installed service and never on a foreground server. `stop` leaves startup at boot enabled. `uninstall` stops the service and removes startup without deleting pools, keys or certificates.

On Windows, let automatic crash recovery finish before stopping the service. If you start it manually and then stop it while a crash restart is pending, the Service Control Manager can start it again when the delay expires. [Microsoft's service restart documentation](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-changeserviceconfig2w#remarks)

To change settings, run `serve install` again with the new options. With no options it keeps the saved settings, installs the current Plasmite executable and starts the service, so the same command upgrades Plasmite. The saved listener addresses persist until you pass `--remote-bind`; a new `SERVER` URL changes the advertised address but keeps the saved listener port. On Windows, a failed update restores the previous service. If that restore also fails, the error names the recovery files; finish the recovery in an administrator terminal before you install again.

The service starts before sign-in only if it can reach the pool directory and network at boot:

- An encrypted or removable volume that unlocks or mounts after sign-in delays startup. On a FileVault Mac, someone must unlock the startup disk before macOS boots.
- On Windows, enable Tailscale's unattended mode if clients need tailnet access before sign-in. On Linux, the `tailscaled` system service can run without a login. On macOS, the GUI client needs a user session. See [Tailscale's startup guidance](https://tailscale.com/docs/how-to/run-unattended).
- To test startup before sign-in, reboot with the owner signed out. A service restart does not test it.

#### Check server status

`plasmite serve status` lists the Plasmite servers running for your OS user, across all pool directories, with each one's directory and local and remote addresses. `serve status --all` adds installed servers that stopped or failed. `--dir` does not narrow either list.

With `--format json`, each row has `pool_dir`, `pid`, `local_url` and `remote_url`; `remote_url` is `null` when the server has no client-facing HTTPS address. `--all` rows also have `managed`, `startup`, `state` (`running`, `starting`, `stopped` or `failed`), `problem` and the saved `setup`. A stopped row has a `null` PID, and its addresses describe saved settings.

`serve logs` shows the installed server's output. If the service fails before the server starts, check the service manager itself; on Windows, Event Viewer's Application log records early errors under the `Plasmite` source.

### Use a trusted certificate

Browsers and AI clients need a certificate they already trust. Use a DNS name that reaches the server and get a certificate for it from an issuer your recipients trust; its Subject Alternative Name must cover that name. Keep the private key on the server:

```console
plasmite --dir ./shared serve https://pools.example.net:9743 --tls-cert /secure/path/fullchain.pem --tls-key /secure/path/privkey.pem
```

From a recipient machine, check the server and its MCP discovery with certificate verification on. In Windows PowerShell, type `curl.exe` instead of `curl`.

```console
curl --fail --show-error https://pools.example.net:9743/healthz
curl --fail --show-error https://pools.example.net:9743/.well-known/oauth-protected-resource/mcp
```

For recipients to install browser trust for a certificate you supply, it must be a restricted server certificate: critical Basic Constraints `CA:false`, digital-signature use without `keyCertSign`, server-authentication use, and a DNS or IP name.

#### Serve behind an HTTPS proxy

If a proxy terminates TLS and presents its own certificate, pass that certificate with `--front-cert`; new access keys then identify the proxy's public key. Configure the proxy to verify Plasmite's backend certificate, to forward only the remote listener, and to preserve the client's `Host` header. Browser writes check `Origin` against `Host`, and `X-Forwarded-Host` does not count.

The server state keeps the `--front-cert` pin after you drop the flag. Before you switch to direct TLS, set the frontend identity to the certificate clients will see, issue keys for it, and check native access before you revoke the old keys.

### Change the certificate or address

Renew a certificate you supply before it expires. An installed Windows service keeps private copies, so run `serve install --tls-cert PATH --tls-key PATH` with the renewed files, plus `--front-cert PATH` behind a terminating proxy. Foreground servers and installed macOS and Linux services read their configured files at each start. The generated certificate keeps its public key across restarts.

| Change | Recipient action |
| --- | --- |
| New certificate, same public key | Native keys keep working. Approve the new certificate again if you used Plasmite's browser trust setup. |
| Different public key | The owner issues new keys; native recipients connect again. Old keys stay valid until the owner revokes them. |
| Different HTTPS address | Connect to the new address. The same key works only if the public key still matches. Authorize MCP clients again. |

A certificate change does not end browser sessions or MCP grants. Revoke old keys to end their access.

### Protect server and client state

The server keeps its identity, key records, browser sessions and MCP grants in `.plasmite-serve` inside the pool directory; on Windows, the installed service keeps its logs there too. Protect that directory and its backups: restoring an older copy restores any access revoked since.

On Windows, use a file system with access control lists, such as NTFS. Plasmite restricts private state to your account, SYSTEM, administrators and the installed service's virtual account. It refuses state with unsafe permissions, hard links, junctions or other reparse points, and it may record the names of the pool path's parent folders so Windows can stop them from moving while the server runs.

Saved client connections live under `%APPDATA%\Plasmite` on Windows, encrypted for your account. `PLASMITE_ACCESS_HOME` selects another directory. If Plasmite reports unsafe ownership or permissions and you cannot confirm the directory's integrity, use a fresh private directory and connect again.

### Use Tailscale

Install and connect Tailscale on both machines. Find the owner's numeric tailnet IP and full MagicDNS name in the Tailscale client and use them in place of the examples below; keep the same full name and port in every Plasmite command. [MagicDNS](https://tailscale.com/docs/features/magicdns) resolves machine names; it does not find Plasmite pools.

Bind the HTTPS listener to the tailnet interface only:

```console
plasmite --dir ./shared serve https://node.tail123.ts.net:9743 --remote-bind 100.101.102.103:9743
```

Invite and connect recipients as in [Share your first pool](#share-your-first-pool), using the tailnet address. For IPv6, bind an address such as `[fd7a:115c:a1e0::abcd]:9743`. Tailnet rules must allow the port; they add no per-pool permissions, and recipients still need an access key. Generated certificates work for native clients; browsers and MCP clients still need [browser setup](#open-pools-in-a-browser).

If you cannot bind the tailnet interface, a [raw TCP Serve forwarder](https://tailscale.com/docs/reference/tailscale-cli/serve) keeps Plasmite's TLS intact:

```console
plasmite --dir ./shared serve https://node.tail123.ts.net:9743 --remote-bind 127.0.0.1:9743
tailscale serve --tcp=9743 tcp://127.0.0.1:9743
```

Run `tailscale serve` in a separate terminal and press Ctrl+C there to stop forwarding. Plasmite presents its own certificate through this route. If you add `--bg` for a persistent route, remove it with `tailscale serve --tcp=9743 --bg off`. Revoke the Plasmite key to end application access.

Never forward the local port `9700`. [Funnel](https://tailscale.com/docs/features/tailscale-funnel) publishes a service to the Internet and needs its own review. A Serve setup that terminates HTTPS must pin the frontend certificate, verify backend TLS and preserve `Host`. Before you rely on a tailnet deployment, connect, feed, follow and revoke from a recipient. [Platform validation](../validation/README.md) lists what Plasmite's own checks covered.

| Symptom | Check |
| --- | --- |
| Cannot reach the server | Both Tailscale clients are connected, the full name resolves, and the IP, port, listener and tailnet rules agree. Use `tailscale ping` for the network and `access status` for Plasmite. |
| Certificate or pin mismatch | The exact advertised name and the certificate presented. A TLS-terminating proxy or a kept frontend pin changes the expected key. |
| Native access works; browser does not | Complete browser trust setup or serve a trusted certificate. Raw forwarding does not terminate TLS. |
| Server reachable outside the tailnet | Check `--remote-bind`. The default listener binds every IPv4 interface. |
| Access survives a Tailscale disconnect | Other network routes to the listener. Revoke the key to withdraw access. |

## Send and read over UDP

Use UDP when a sender can live with a message being lost, duplicated or reordered, such as frequent sensor readings. `--udp` adds a listener to the same server and directory:

```console
plasmite --dir ./shared pool create sensors
plasmite --dir ./shared serve --udp
plasmite feed udp://127.0.0.1:9701/sensors '{"temp_c": 21.4}'
```

Bare `--udp` listens on `127.0.0.1:9701`; `--udp=IP:PORT` binds another numeric address. Each datagram carries one complete message and must fit in 65,507 bytes, including the pool name. See the [UDP input contract](../../spec/udp/v0/SPEC.md) and the [sender examples](../cookbook.md#collect-sensor-readings-over-udp).

The listener can feed every pool in the directory, including pools created later, and it checks no credentials. A non-loopback bind gives that access to every sender that can reach it, so bind one only on a network whose rules limit senders. Use HTTPS when a sender needs authentication or confirmation that the server stored its message.

`--udp-follow` also lets clients `fetch`, `follow` and `duplex` over `udp://HOST:PORT/POOL`, and implies `--udp`. Any client that reaches the socket can then read every pool. Plasmite first checks that the client receives replies at its address, which stops spoofed requests from aiming large replies at a victim; the check proves no identity. Readers receive messages in arrival order, including late messages and duplicates, and a `fetch` can end with messages still in flight. The [UDP transport contract](../../spec/udp/v1/SPEC.md) covers leases, controls and notices. `--max-tail-concurrency` limits UDP followers separately from HTTP followers, and a slow follower never holds up input.

An installed server keeps its UDP settings through restarts and updates. Enable input with `serve install --udp` and reads with `serve install --udp-follow`. `serve install --no-udp-follow` withdraws reads and keeps input; `serve install --no-udp` removes both.

## Connect an AI client

A local MCP client runs `plasmite mcp` on the machine that holds the pools; see the [cookbook](../cookbook.md#give-an-ai-agent-your-pools). For a shared server, connect the client to the server's HTTPS `/mcp` address. The recipient needs no local Plasmite, but both the AI client and the browser it uses for login must trust the server certificate, so first [deploy a trusted certificate](#use-a-trusted-certificate).

For Claude Code:

```console
claude mcp add --scope user --transport http pools https://pools.example.net:9743/mcp
claude mcp login pools
```

For Codex CLI:

```console
codex mcp add pools --url https://pools.example.net:9743/mcp --oauth-client-registration dcr --oauth-resource https://pools.example.net:9743/mcp
```

The client opens an authorization page that shows the server, the client, the requested `/mcp` address and the callback. Enter the access key and approve the client. Plasmite then issues renewable credentials. Ask the client to list pools and read `chat` to confirm access. Changing the server's address requires fresh authorization. To sign in again for an existing Codex entry, run `codex mcp login pools --oauth-client-registration dcr`. Claude Code also offers login through `/mcp` in an interactive session.

## Troubleshoot a connection

Start with:

```console
plasmite access status https://pools.example.net:9743
```

The report answers three questions: did this OS account save credentials, can it reach the server, and does the server accept its key? On macOS and Windows it also reports the certificate's browser trust state and fingerprint.

| Symptom | What to check or do |
| --- | --- |
| Server unreachable | The hostname, port, running server, and network and firewall route from the recipient machine. |
| No saved credentials | Run `access connect` as the OS account that runs the command. |
| Key rejected | Ask the owner to check `access keys` for revocation and confirm the key belongs to this server. |
| Certificate name, expiry or public-key mismatch | Check the exact address and ask the owner to fix the certificate or issue a key for its new public key. Keep verification on. |
| Native access works; browser rejects the certificate | Reconnect in an interactive terminal and complete browser trust setup, or serve a certificate the browser already trusts. Check expiry and device policy. |
| Browser works; MCP fails before login | The AI client needs certificate trust too. Serve a certificate both trust. |
| Browser setup fails or you decline it | The native connection still works. Retry in a signed-in interactive terminal. |
| Windows reports unsafe state permissions | Use NTFS and inspect the named directory. Keep Plasmite's private ownership and permissions. |

## Reference

Run `plasmite serve --help` and `plasmite access --help` for every option. See the [CLI guide](../cli.md) for pool addresses and output, and the [contract](../../spec/v2/SPEC.md) for [HTTP](../../spec/v2/SPEC.md#http) and [MCP](../../spec/v2/SPEC.md#mcp) behavior.
