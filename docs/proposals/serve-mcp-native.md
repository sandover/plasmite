# Sharing Plasmite Pools Securely

Status: proposal

## The core model

An **owner** runs a server for a directory of pools they own and shares access by giving someone its address and an **access key**. The **address** tells a client where to connect. The key permits reading, appending, creating, and deleting pools throughout the directory, including pools created later.

The key also includes the information clients need to verify the server. Keys remain valid until revoked and can be reused across interfaces and machines.

Plasmite offers three interfaces. Agents use MCP (Model Context Protocol) through a **harness**, such as Codex CLI or Claude Code.


| Interface                        | How the recipient connects securely                                                                                          |
| -------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| CLI or API                       | Supply the server address and access key.<br />Plasmite saves server trust and credentials.                                  |
| Web                              | Open the trusted HTTPS address and enter the access key.<br />The browser saves credentials.                                 |
| MCP, through installed Plasmite  | The harness launches Plasmite locally.<br />Plasmite uses the CLI’s saved connection.                                       |
| MCP, without installing Plasmite | The harness connects to the server’s MCP address and opens browser authorization.<br />The harness saves OAuth credentials. |

## Assumptions

- The owner and recipient have a private channel for delivering the access key.
- All local machine processes are trusted. Using pools locally is credential-free.
- Connections between machines use HTTPS.

## Start the server

The owner runs:

```console
plasmite --dir ./shared serve
```

This starts the pool API, web interface, and remote MCP endpoint. Local use is available immediately at `http://localhost:9700`.

Plasmite creates and retains its own HTTPS server certificate, covering only the shared addresses and marked as unable to issue other certificates. Plasmite clients on other machines verify the server during the setup phase, detailed below.

Installed Plasmite can establish browser trust for Chrome and Safari on macOS, and Chrome and Edge on Windows. Other browsers and direct MCP harnesses need a certificate they already trust, supplied by the owner or an HTTPS reverse proxy.

### Requirements

- The local page links to `/access`, which lists access keys and lets the owner create and revoke them. Administration stays on the local listener, available to all trusted local processes.
- Startup prints the shared directory, pools, local page, and configured remote addresses. The CLI and Access page identify incomplete setup and explain the next step; local use remains available.
- Server settings are available through configuration as well as the local page, so headless setup needs no browser.
- An HTTPS-terminating proxy is part of the trusted server: it receives client credentials and pool data. The owner supplies its public-facing certificate in Plasmite's configuration. The proxy forwards only to the authenticated service and verifies its HTTPS certificate.

## Connect with Plasmite installed

**1. The owner creates an access key.**

```console
plasmite --dir ./shared access invite --name "Alex"
```

The command displays the key. The owner can also create one through "Invite" on the local Access page.

**2. The owner supplies the address and delivers the key privately.**

For example: “Connect to `https://pools.example.net:9743/` using this key.” The address can be shared openly; the key is private.

**3. The recipient connects.**

For the CLI, run:

```console
plasmite access connect https://pools.example.net:9743/
```

The command prompts for the key with hidden input and uses it to verify the HTTPS server before authenticating. After checking access, it saves server trust and credentials for the current OS user. The API offers the same operation directly.

## Connect in a browser

The recipient opens the server's HTTPS address in a browser that trusts its certificate. If the browser already trusts it, no Plasmite installation is needed.

After verifying the server, `plasmite access connect` offers browser trust setup for Chrome and Safari on macOS, and Chrome and Edge on Windows. The user approves through the operating system's standard prompt, then Plasmite opens the original HTTPS address. Setup adds the server's certificate to the current user's trust store. The supported browsers trust that exact certificate for its named addresses.

The recipient enters the access key on the page. Once accepted, the browser saves credentials for later visits; clearing browser storage removes them.

With exact-certificate trust, a renewed or replaced certificate requires another approval, even if its public key stays the same. An expired certificate blocks browser access until the server renews it and the user approves it. The access key remains valid.

The local page also offers access administration without credentials. The remote page offers only pool operations. The browser sends the access key only to the HTTPS address the user opened. The key never appears in URLs or browser history.

## Connect through MCP

### With Plasmite installed

After saving the connection, `plasmite access connect` prints setup commands for Claude Code and Codex CLI. The user or agent runs the appropriate command; the harness manages its own configuration.

When starting Plasmite's tools, the harness launches:

```console
plasmite mcp --remote https://pools.example.net:9743/
```

This local process translates MCP calls into native requests using the saved server trust and access key. It runs under the OS account that saved the connection. Harness configuration contains no credentials.

### Without Plasmite installed

The harness and browser must already trust and reach the server's HTTPS address. The access key cannot establish that trust for the harness or browser.

1. The recipient adds `https://pools.example.net:9743/mcp` to their harness.
2. The harness opens or links to Plasmite's authorization page. The recipient pastes the access key and approves access for the named harness and directory.
3. Plasmite issues OAuth credentials tied to the access key. The harness saves and renews them; revoking the key ends access and renewal.

Agents can later install Plasmite and use the native CLI or API with the same address and access key.

## Saved connections

Commands and API calls select saved connections by destination, so several connections can coexist. Saved connections belong to the OS account running the client, including background services on headless Linux. Native clients use their access keys across reboots and long idle periods without renewal. Direct MCP harnesses renew their OAuth credentials without human approval.

Check a saved connection:

```console
plasmite access status SERVER_URL
```

Status shows the destination, whether credentials are saved, reachability, and whether the server accepts the credentials. On macOS and Windows it also reports installed browser trust and certificate expiry.

Repeating setup with the same key reuses the connection. A different key replaces saved credentials only after validation and storage succeed, without revoking the previous key. An unreachable server leaves saved credentials intact.

Follow [GitHub CLI](https://cli.github.com/manual/gh_auth_status) and [Tailscale](https://tailscale.com/docs/reference/tailscale-cli) in making state clear and errors actionable. Across the CLI, API, and web, errors identify the destination, failing step, and next action. Distinguish name resolution, connectivity, certificate trust, missing credentials, revoked access, and uncertain writes. Scripts receive stable machine-readable results; background commands never wait for interactive repair.

## Disconnect, revoke, and recover

The local Access page and `plasmite access` commands share an administration API for creating, listing, and revoking keys. The page offers Invite and Revoke; the CLI offers `invite`, `keys`, and `revoke`, with `--dir` selecting the local server. Lists show names and approximate activity, never secrets.

Before reporting successful revocation, the server records it durably, blocks new operations, and closes the key's active streams, including idle ones. Individual writes already accepted may finish within their deadlines. Data already handed to the network may still arrive. This applies to native clients, browser sessions, and both MCP paths; other keys keep working.

A client can forget its saved connection:

```console
plasmite access disconnect SERVER_URL
```

Disconnect removes the saved connection used by both the CLI and local MCP processes. Browser disconnect ends the session and removes its cookie. Direct MCP disconnect or OAuth token revocation ends only that harness’s authorization. The key and its other uses remain valid. Browser certificate trust has a separate CLI removal action.

The same key can recover a failed setup or lost saved connection. If all copies are lost, the owner revokes it and creates a replacement. If creation succeeds but its response is lost, the owner finds the entry in the key list and replaces it the same way. Keys and revocations survive restarts.

## Implementation

One process runs the pool server, key checks, web interface, remote MCP, OAuth service, and local administration. Both MCP paths share tool definitions. The local MCP process calls the native client, sharing saved connections and server verification with the CLI; remote MCP calls the server’s pool operations directly.

The protocol contract must settle key format, authenticated native requests, and browser sessions. Use established libraries for credential protection and OAuth.

### Server configuration and state

Local HTTP uses port `9700`; secure serving uses `9743`. Both allow overrides and report busy ports as errors. Local HTTP stays on loopback.

Each directory has one running server. If its address changes while its TLS public key stays the same, native clients reconnect explicitly with the same access key, and local MCP configurations update their destination. Direct MCP requires fresh OAuth authorization because it binds to the issuer and exact MCP resource URL. Clients never forward saved credentials automatically to a new address. Restarts and internal listener changes preserve access.

An access key identifies the TLS public key presented at the client's destination. For direct serving, this is Plasmite's retained key. When a proxy terminates HTTPS, it is the proxy's key, taken from the certificate the owner supplied. Plasmite never learns a replacement identity from an unverified connection.

Renewing a certificate with the same public key preserves native trust. Replacing that public key requires the owner to issue fresh access keys for native clients. Existing permission records and browser or OAuth sessions remain valid until explicitly revoked. A fresh access key contains a new secret; the server cannot recover an old secret from its stored verifier.

The owner-protected `.plasmite-serve` directory holds server identity, configuration, and access records beside the pools. Startup fails if it cannot protect that state or acquire the directory's instance lock. Pool APIs never expose private server files.

Serving state moves with the directory and carries its access permissions. Restoring an old backup can restore revoked keys. An independent server needs fresh serving state.

### Key storage and enforcement

An access key contains an **access secret** and a SHA-256 fingerprint of the client-facing certificate's **public key** (SPKI). Browser sessions and OAuth credentials refer to key records. Native clients, including local MCP processes, authenticate with the saved access secret.

- Use high-entropy secrets and established cryptographic libraries. Store a verifier for each access secret on the server; protect client credentials at rest.
- Send a saved access secret only to the destination the user configured. Verify the certificate's public key, hostname, validity, and TLS proof of possession before sending the secret. A trusted certificate issuer does not override a public-key mismatch. Never forward secrets to a different destination through a redirect.
- Commit a new key before displaying it. Concurrent connections can use the same key; they do not create duplicate key records.
- Coordinate revocation with admission and stream output. Discard pool data still queued inside Plasmite and close idle streams without waiting for another message or the normal timeout. Each remote operation, including each record in a streaming write, checks current permission independently of connection reuse.
- Keep secrets out of arguments, routine logs, public descriptions, and MCP discovery. Keep pool contents and private access records behind their access checks.
- Protect local HTTP against cross-origin and DNS-rebinding attacks, including forged administration requests. Render pool content as inert data and load no third-party scripts. Protect browser cookies with Secure, HttpOnly, and appropriate SameSite settings and prevent forged browser writes. Bound request sizes, authentication attempts, streams, and waits.
- Authorization recovery never silently repeats a write.

### MCP interface

Use the published [MCP 2026-07-28 specification](https://modelcontextprotocol.io/specification/2026-07-28) as the prototype baseline and pin the tested revision in the public contract. Local MCP uses standard input and output; direct connections use the server's HTTPS MCP endpoint.

Tools provide explicit inputs, structured results, useful errors, and destructive-operation annotations. Connection failures identify the destination and next action. A running local MCP process must respect disconnect and credential replacement before admitting another tool call.

Direct MCP follows the [MCP authorization profile](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization):

- Provide discovery metadata, authentication challenges, and client identification and registration supported by the selected harnesses.
- Use authorization code with Proof Key for Code Exchange (PKCE). Bind approval to the harness, exact callback, code challenge, issuer, and MCP resource. Show the requested directory access; a harness's declared name does not prove its identity.
- Tie short-lived OAuth tokens and renewable credentials to the access key. Check that key during code exchange, renewal, and every pool operation. Revoking it stops access and renewal through every associated authorization.
- Use established libraries for protocol validation, replay protection, and bounded client-metadata retrieval. Saved authorizations and renewal survive server restarts.

MCP documentation resources can draw from the same versioned sources as CLI help and guide agents through installing and connecting the native client.

## Prototype checks

Use the prototype to verify these workflows:

1. **Three-step onboarding:** create and deliver one access key, then use it through the CLI, API, and remote web page. Verify a visible test message and saved connections across client restarts. The address and access key are supplied separately.
2. **Continuing use:** two saved destinations work independently. Restart clients and servers, run under a background-service account, and check status. Disconnect one client without affecting other uses. Revoke a shared key and verify that every CLI, browser, and MCP client using it loses access while other keys continue. Idle time alone must not end access.
3. **Recovery:** exercise repeated connections with the same key, lost responses, credential-save failure, cleared browser storage, replacement of a saved key, and disconnect with the server unavailable. Confirm that authorization recovery never silently repeats a write. Verify browser-session protections and revocation during concurrent requests. Check every message for a clear next action.
4. **Deployment:** publish a complete HTTPS recipe covering address setup, certificate renewal, and remote reachability. Verify local-only administration and continued native access after same-key certificate renewal.
5. **Local MCP:** follow the printed setup in Claude Code and Codex CLI, then read and write pools using the CLI's saved connection. Use one harness for the fuller restart and recovery run. Check that harness configuration and tool output contain no secrets.
6. **Direct MCP:** connect from the same two harnesses without installing Plasmite, authorize in the browser, and read and write pools. Use Claude Code for the fuller saved-credential and unattended-renewal run. Reuse protocol tests for cancellation, invalid exchanges, and revocation.
7. **Browser trust:** verify CLI-led setup and removal in Chrome and Safari on macOS, and Chrome and Edge on Windows. Accept the intended certificate and addresses; reject changed or expired certificates and certificates signed using the trusted certificate’s key. Verify OS approval, standard-user setup, browser restart needs, and the effect on other software using the same trust store. Keep native access independent of browser setup.

Choose the cheapest reliable check for each consequential failure and reuse evidence across interfaces. Verify that an agent can start through direct MCP, then install Plasmite and continue on the same pools.

Deliver a measured performance comparison: for 512-byte messages and one larger size, report local and direct MCP throughput as percentages of native client throughput, with reads and appends shown separately. Use matching workloads and warm connections on the same machines and network, excluding model response time. Keep latency, variation, and reproducible commands with the results so we can publish a concise, defensible explanation of the tradeoffs.

Test an address change by reconnecting with the same access key and updating the MCP destination. Verify that CLI and local MCP use the new saved connection, while direct MCP completes fresh authorization for the new address.

The implementation ships as a new major version after the public contracts describe the validated behavior. A separate real Raspberry Pi exercise (`Q5KJQK`) will inform the onboarding experience; it does not gate implementation or release. Raspberry Pi packaging and the other general onboarding topics remain consultation tasks in Ergo epic `5FEBG4`.
