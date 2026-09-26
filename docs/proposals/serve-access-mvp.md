# Access for `plasmite serve`

Status: superseded design history. [Sharing Plasmite Pools Securely](serve-mcp-native.md)
sets the current direction. Commands, addresses, and defaults below describe
the earlier proposal and may differ from the implemented behavior.

## Share a directory with collaborators

Plasmite should keep its easy, credential-free use on a trusted machine while making it straightforward to invite collaborators over an untrusted network. The owner starts a server, sends an invitation through an existing private conversation, and the recipient connects. Plasmite remembers the connection so ordinary work can continue without another invitation.

## How access works

The **shared directory** is the unit of access. Sharing it gives a collaborator full use of every pool inside it, including pools created later. Owners who need separate groups of collaborators use separate pool directories.

The server trusts its local machine. Local programs can use pools and manage access without credentials, and programs with filesystem access can continue to use pools directly. The **owner** is the person or process operating the server. Remote collaborators connect through the secure listener and receive pool access; administration stays local.

An **invitation** lets its recipient establish one remote connection. The owner sends it through an existing private channel. It carries the server's address, the information needed to verify that server, and a short-lived secret. Accepting it consumes the invitation and creates a **grant**: the server's lasting record that this client may use the directory. The owner can revoke each grant independently.

The client keeps a **saved connection** containing the server's address, how to verify it, and a secret credential for its grant. On later requests, Plasmite uses that credential to prove it holds an active grant. An unused invitation expires quickly. Once the client accepts it, the grant and saved connection let the client return after restarts. Revoking the grant ends access through that connection.

## Start once, get two addresses

The owner runs:

```console
plasmite --dir ./shared serve
```

The server starts two listeners with standard ports:


| Address                  | Purpose                                                                     |
| ------------------------ | --------------------------------------------------------------------------- |
| `http://localhost:9700`  | Trusted local use: pool operations, the web interface, and administration   |
| `https://localhost:9743` | Secure client connections: invitations and the remote access they establish |

The local listener always stays on loopback, so other machines cannot connect to it directly. The secure listener also starts on loopback; `--bind` lets the owner make it reachable on a network interface. Both listeners work with the same pool directory.

The ports are predictable defaults: `9700` for local use and `9743` for secure connections, with `43` echoing the standard HTTPS port, `443`. Owners can explicitly choose others when running several servers on one machine. If a requested port is busy, startup fails and explains the conflict. Startup prints the resolved directory, both addresses, and the admin-page URL.

The **admin page** is part of the local web interface, at `http://localhost:9700/access` by default. It lets the owner create invitations, see clients, and revoke access. Opening `http://localhost:9700` shows the pools, with **Access** in the navigation. Humans and agents can find administration at a known address; `plasmite access open` is a convenience for opening it.

We trust all local processes to use pools and administer remote access. The operating-system user who starts the server owns its files. All local callers have the same access to administration. Local administration commands use the local HTTP API.

A website open in the browser does not inherit that local trust. The local listener must reject requests from unrelated websites, including attempts to change access or write to pools. The secure listener never offers administration, and a reverse proxy must never publish the local listener.

### Routes people can remember

The base address opens the pool browser. Pages have short, stable paths, while programmatic operations live under the versioned `/api/v1` prefix.

| Route | Purpose |
| --- | --- |
| `/` | Browse the shared pools |
| `/pools/:name` | Open a pool |
| `/access` | Manage clients and invitations; local listener only |
| `/connect` | Accept an invitation; secure listener only |
| `/api/v1/…` | Programmatic operations |
| `/mcp` | MCP connection |

Both listeners use the same paths for pool operations. The listener determines how Plasmite checks access. The browser and CLI use the same pool API. On the secure listener, visitors who have not connected see a way to accept an invitation before they can browse pool data. Invitations carry the complete secure address so recipients do not need to remember its port.

## Invite someone and let them return

The owner creates an invitation from the admin page or the CLI:

```console
plasmite --dir ./shared access invite [--name NAME]
```

They can add a name such as “build agent” or “Alex's laptop.” Plasmite shows the directory being shared and explains that the invitation gives full pool access, including pools created later. The invitation is one copyable block that works once and expires in 15 minutes.

The owner sends the block through a private channel they already use with the recipient: a message, an agent assignment, an SSH connection, or a private shared folder between a host and virtual machine. Someone who needs access asks there, and the owner replies with an invitation.

The recipient runs `plasmite access connect` and pastes the invitation at the prompt; agents provide it on standard input. Reading the invitation from standard input keeps its secret out of shell history, process arguments, and command-line logs. The invitation tells Plasmite where to connect and how to verify the server. After checking the server, Plasmite redeems the invitation for a grant and saves the connection. It reports success only after saving it.

Supported remote commands use the saved connection automatically. Plasmite handles the credential behind the scenes.

The client can return after either machine restarts. Its access lasts until the owner revokes it. A name helps the owner recognize the connection; it does not prove who is using it. Anyone holding an unused invitation can redeem it, and anyone holding a copied client credential can use the same grant. The delivery channel therefore needs to protect the invitation from interception and replacement.

## What sharing permits

Every invited collaborator gets full pool access throughout the directory: listing and inspecting pools, reading and following messages, adding messages, and creating or deleting pools. Administration stays on the trusted local listener.

Inviting someone means trusting them with the shared space. The invitation view explains that they can delete pools and that appending to a bounded pool can evict older messages. Owners who need separate groups of collaborators use separate directories.

Local callers have full pool access. On the secure listener, an active grant gives the client that same pool access until the owner revokes it.

## Use the connection from different tools

Native commands, the browser, and Model Context Protocol (MCP) clients use the same grants and pool operations. Each interface uses the same access checks.

- **Native Plasmite clients** enroll with `plasmite access connect`. Supported remote commands such as `feed`, `follow`, and `duplex` then use the saved connection without repeated token or certificate flags.
- **Remote browsers** open `/connect` at the secure address and paste an invitation. The browser remembers its connection for that site. Browser access requires an HTTPS address and certificate the browser trusts; server-key pinning remains a native-client feature. The local web page continues to work without an invitation.
- **MCP hosts that can run a command** use `plasmite mcp --remote URL` after enrollment on that machine. Plasmite handles the connection and credential, so the host's configuration contains no secret. Local `plasmite mcp` continues to use pools directly.
- **HTTP-only apps and MCP hosts** can use `plasmite access connect --output PATH` to redeem their invitation into a protected connection file. The operator puts the resulting credential into the app's secret configuration. Each app that needs independent revocation gets its own invitation.

The export helper uses the same invitation exchange as every other remote client. It writes the connection to a new file instead of the normal client store and reports the file's location without printing its secret. Apps must support the invitation's server-verification method; ordinary HTTP apps generally need trusted HTTPS.

MCP exposes the full set of pool tools to admitted clients. For secure connections, it verifies that the grant is still active when admitting each tool call. These integrations do not yet provide the OAuth flow described in the [MCP authorization specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization).

### Choose an address the recipient can reach

When creating the first invitation, Plasmite suggests addresses and asks which one the recipient can reach. It remembers the choice, and scripts can supply `--url URL`. An invitation always contains a usable address, never a wildcard such as `0.0.0.0`. The owner provides a network path the recipient can use.

A server creates and retains its own Transport Layer Security (TLS) key and certificate by default. Native clients can verify that key using information carried in the invitation. Owners who need browser access can supply a trusted certificate or use an existing HTTPS reverse proxy. Certificate configuration supports the same invitation and grant model in either deployment.

A saved connection belongs to an exact address: scheme, host, and port. Each secure address serves one shared directory. A new address gets a new invitation. Replacing a pinned server key or changing the verification method also requires a new invitation. Ordinary certificate renewal continues to work when it satisfies the saved verification rule.

## Manage access and recover from mistakes

The admin page provides **Clients**, **Invite**, and **Invitations**. Clients includes disconnected clients, their names, and an approximate last-use time. The owner can revoke a client or cancel an unused invitation.

The corresponding commands under `plasmite access` are `open`, `invite`, `invitations`, `cancel INVITATION_ID`, `clients`, and `revoke GRANT_ID`. Together with `connect`, these commands give access management one home under `plasmite access`. `serve` starts the server. Commands that return data provide stable `--json` output. For administration, `--dir` selects the running local server; the CLI can discover an explicitly configured local port from the server's state in that directory. `connect` gets the remote address from the invitation.

An invitation's secret appears once. Losing an unused invitation means cancelling it and creating another. If the client loses the enrollment reply or cannot save its credential, the server may already have created a grant. The error includes the invitation ID so the owner can find that grant, revoke it, and send a replacement.

Replacing a saved connection requires an explicit `--replace` and preserves the old connection until the new one saves successfully. Removing a client file or clearing browser data does not revoke the server's grant. Owners revoke old grants when they are no longer needed.

Revocation blocks new operations once the server reports success, and it survives a restart. Work already accepted may finish. Existing streams and waits retain their original deadlines; reconnecting checks access again. The admin page explains this limit so revocation does not promise to recall data already sent or undo an accepted write.

## Keep state with the shared directory

The server keeps keys, invitations, grants, and local discovery information in an owner-protected `.plasmite-serve` directory alongside the pools. One pool directory has one running server.

Restarting preserves invitations and grants. Moving the complete directory while the server is stopped carries its access state with it. A changed network address still requires new invitations on clients.

A backup includes the server's authority as well as its data. Restoring old access state can restore grants that were revoked after the backup. To create an independent shared space, copy only the desired pools into a fresh directory and let the server create new state. Pool listings and APIs never expose the private metadata directory.

## Implementation notes

The brief fixes the behavior people can depend on. The implementation can choose storage formats, locking, and internal organization without adding concepts to the user experience.

### Keep the responsibilities small

- Keep invitations, grants, and saved connections as distinct records. Invitation expiry concerns enrollment; an active grant admits a client to the directory; endpoint verification establishes which server the client trusts.
- Use one shared set of operations for creating and redeeming invitations, listing clients, revoking grants, and admitting pool operations. The CLI, web page, HTTP API, and MCP translate inputs and results for their users.
- Keep names and approximate last-use times out of access decisions. Updating activity information need not force a durable write on every request.
- Consuming an invitation and creating its grant must commit together, before returning the credential. Concurrent redemption creates one grant; cancellation racing redemption has one winner.
- Coordinate revocation with admission so an earlier credential check cannot admit new work after revocation succeeds. Persist access changes before reporting success. Client-side saving is a separate step with the recovery described above.

### Access API

The admin page and CLI use these local API operations. Redeeming an invitation uses the secure listener.

| Operation | Request | Listener |
| --- | --- | --- |
| Create an invitation | `POST /api/v1/invitations` | Local |
| List invitations | `GET /api/v1/invitations` | Local |
| Cancel an invitation | `DELETE /api/v1/invitations/:id` | Local |
| List clients' grants | `GET /api/v1/grants` | Local |
| Revoke a grant | `DELETE /api/v1/grants/:id` | Local |
| Redeem an invitation | `POST /api/v1/invitations/redeem` | Secure |

### Protect each connection

- Secure requests require HTTPS and an active grant for pool access, even when a client reaches the secure port through loopback. The local port remains credential-free. Unauthenticated secure visitors can enroll but cannot inspect pools, clients, or administration.
- Invitations specify either normal HTTPS certificate and hostname verification or an exact server public-key pin. Both require the invited scheme, host, and port. Neither verification method falls back to the other. Check the server before sending any secret; never forward credentials through redirects or provide a verification-bypass option.
- Reverse-proxy invitations describe the client-facing, trusted HTTPS address. The proxy separately verifies its secure backend connection and forwards client authorization headers. A backend key cannot authenticate the proxy to the client. Never proxy the trusted local port.
- Protect the local HTTP API against cross-origin websites and DNS rebinding, including reads, writes, and admin actions. Validate the local Host and browser origin, reject cross-site requests, and require request forms that an unrelated website cannot submit. Do not rely on Cross-Origin Resource Sharing (CORS) alone. These checks must keep native local clients working without login credentials or fabricated browser-origin headers.
- Store only verifiers for invitation and grant secrets on the server. Protect client credential files with per-user filesystem permissions. Keep secrets out of process arguments, URL parameters, and routine logs; refuse to overwrite an export file.
- Remote browsers accept only invitations for their own trusted HTTPS origin, save their grant for that site, and send credentials in authorization headers, including on streams. They cannot enforce server-key pins and must not reinterpret pinned invitations. Render messages and labels as inert content, load no third-party scripts, and use a restrictive Content Security Policy on both local and remote pages.
- Bound invitation attempts, request sizes, and concurrent operations. Keep finite deadlines: the default maximum HTTP stream duration is 30 seconds, configurable by the owner; MCP waits last at most 60 seconds. Show the configured HTTP limit when explaining revocation. Bytes already buffered in transit may arrive later. Admitted collaborators receive no guarantee of fair resource allocation.
- Recreate runtime locks and discovery information as needed without resetting durable access records. Refuse startup if the server cannot protect its private state or acquire the directory's instance lock.

## Release scope and checks

This model ships as a breaking change in the next major version. Trusted local use stays credential-free. Remote clients use invitations and saved grants.

Before release, verify that:

- Bare `serve` starts local HTTP on `9700` and secure HTTPS on `9743`, with explicit port overrides. The root page links to `/access` locally and guides unenrolled secure visitors to `/connect`. Local clients and administration need no credentials. The local listener stays on loopback and rejects unrelated websites; the secure listener exposes no administration.
- A collaborator can receive an invitation, connect once, and return after restarts using each supported client type. New addresses require new invitations, and a mismatched server never receives a secret.
- Local callers and clients with an active grant can use every pool operation throughout the directory, including on pools created later. Administration stays on the local listener.
- Concurrent redemption, cancellation, interrupted enrollment, and failed client saves follow the stated one-use and recovery rules.
- Revocation blocks new work, survives crashes, and leaves other grants working. Existing work follows its original deadlines.
- The CLI and web page produce equivalent access changes, and HTTP and MCP agree on grant checks. Hostile pool content cannot execute in either web page, and pool APIs cannot expose private state.
