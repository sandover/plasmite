# Plasmite MCP Contract

Plasmite implements the [Model Context Protocol (MCP) specification dated
2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25). Local
standard input and output (stdio) and remote HTTPS expose the same tools,
resources, inputs, and results through one dispatcher.

## Lifecycle and methods

Clients begin with an `initialize` request. Plasmite answers with protocol
version `2025-11-25`, tool and resource capabilities, server information, and
instructions. Clients then send `notifications/initialized`; Plasmite accepts
the notification without replying. `ping` returns an empty result.

The dispatcher supports `tools/list`, `tools/call`, `resources/list`, and
`resources/read`. It does not create an MCP session ID or keep client state
between requests. It does not support prompts, resource subscriptions, or
server-sent events. Tool calls and resource reads use the standard 2025-11-25
message shapes; requests do not need Plasmite-specific `_meta` fields, and
results do not add protocol-specific cache fields.

## Local stdio

`plasmite mcp` uses local pools. `plasmite mcp --remote SERVER_URL` uses the
saved native connection for that exact HTTPS destination. Plasmite reads the
current saved credential before each tool operation, so disconnecting,
replacing, or revoking the credential affects the next call. Harness
configuration contains the command and server address, not the access key.
Local MCP does not use OAuth.

The process reads newline-delimited JSON-RPC from standard input, writes only
JSON-RPC messages to standard output, and exits when standard input closes.

## Remote HTTPS

The MCP endpoint is the exact configured `https://HOST[:PORT]/mcp` URL. Each
client message uses its own `POST`, and the client advertises both
`application/json` and `text/event-stream` in `Accept`. The initialize request
may omit `MCP-Protocol-Version`; later requests must send
`MCP-Protocol-Version: 2025-11-25`. Plasmite returns JSON for requests and
`202 Accepted` with an empty body for notifications. It does not issue an
`MCP-Session-Id`; `GET` and `DELETE` return `405`.

Plasmite validates a supplied `Origin` against the HTTPS host. A missing or
unsupported protocol-version header on a later request returns `400`. An
unknown method returns `404` with JSON-RPC code `-32601`. The request body is
the source of the method and tool or resource name; clients do not mirror them
in extra headers.

Remote HTTP MCP requires an OAuth bearer access token. An unauthenticated
request returns `401` with a `WWW-Authenticate` link to protected-resource
metadata at `/.well-known/oauth-protected-resource/mcp`. Authorization server
metadata lives at `/.well-known/oauth-authorization-server`.

## Authorization

The server accepts dynamic public-client registration at `/oauth/register`.
The client supplies a name and exact redirect URIs. The name is a label, not
proof of client identity. Redirects use HTTPS or loopback HTTP. Unapproved
registrations expire after ten minutes. The registration endpoint limits each
source address to 30 attempts per minute.

Authorization requires an exact MCP resource URL and an S256 Proof Key for
Code Exchange (PKCE) challenge. The server presents a browser page at
`/oauth/authorize`; the user enters one access key and approves the named
client. The page posts the key only to the server origin. The authorization
code is single use, lasts ten minutes, and binds the client ID, exact callback,
issuer, resource, and PKCE challenge. The callback includes `iss`.

`/oauth/token` issues a bearer access token valid for fifteen minutes and a
rotating refresh token. A refresh token identifies one grant family. Reusing
an older token ends that family. Each registered client has one live grant;
approving it again ends its prior grant. Renewal replaces the prior access
token, so a client must use the newest pair. `/oauth/revoke` ends the grant
family when given either its access or current refresh token. Both token kinds
are stored as hashes in the owner's private serving state. Their records name
an access key ID, never the key secret.

Every MCP operation checks the access key's current revocation state. Key
revocation stops new work and idle waits for browser, native, local MCP, and
direct MCP clients that share it. Refresh and code exchange also fail after
revocation. Token revocation blocks new MCP calls; an already admitted wait may
complete within its 60-second cap. Key revocation cancels active waits. An
access key for another server cannot approve this one.
