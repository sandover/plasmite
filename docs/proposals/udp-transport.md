# UDP feed and follow for Plasmite

Status: implemented on main. This document records the design. The
[UDP v1 specification](../../spec/udp/v1/SPEC.md), [serving guide](../record/serving.md#send-udp-messages),
and SDK READMEs describe the current contract. Existing append-only senders keep
working.

## What a user does

A user runs one server over one pool directory, then chooses a transport in the
pool reference:

```sh
plasmite serve --udp-follow
plasmite feed udp://127.0.0.1:9701/sensors --data '{"temperature":23}'
plasmite follow udp://127.0.0.1:9701/sensors
plasmite follow udp://127.0.0.1:9701/sensors --tail 10
plasmite follow udp://127.0.0.1:9701/sensors --tail 100 --no-follow
```

`--dir` keeps its existing meaning and default. The same server continues to
serve HTTP and HTTPS. `serve --udp=IP:PORT`, installed settings, and `serve
install --no-udp` keep their current syntax and behavior. Add one server
capability switch, `--udp-follow`, which enables reads and implies UDP input at
its existing/default address. For a custom endpoint use `serve --udp=IP:PORT
--udp-follow`. Existing `--udp` stays input-only.

Installed configurations default follow to false, including old saved settings.
`serve install --udp-follow` opts in; `serve install --no-udp-follow` withdraws
reads while retaining input. Omitting either preserves the saved choice.
`--no-udp` removes both capabilities. Removing follow cancels active
subscriptions. This extra switch prevents an upgrade from exposing previously
write-only pools. The client must name the UDP port; it never derives that port
from an HTTP URL.

A sender submits one message per datagram. A follower asks the server to send
pool messages to its socket. Neither operation acknowledges individual message
delivery. UDP may lose, duplicate, or reorder datagrams. The client never
repairs missing data through HTTP and never quietly changes transports.

The pool still commits accepted writes through the existing append path and
assigns their sequence numbers and timestamps. Network loss leaves stored
messages intact; a lost input datagram creates no stored message. A later local
or HTTP read can inspect what the pool retained.

This feature promises feed and follow, including best-effort selected history,
through the CLI and supported native SDKs. It does not promise faster
performance. It adds no pool administration, fetch-by-sequence, multicast,
automatic retries of writes, reliable delivery, encryption, or public-Internet
transport.

## The CLI surface

The URL supplies the transport. Add no `--transport` or `--udp` flag to message
commands, second server command, format flag specific to UDP, or saved UDP
credentials.

Accept `udp://HOST:PORT/POOL`: one decoded pool-name segment, explicit port,
optional bracketed IPv6 host, no user information, query, fragment, or
API-shaped path. Apply the existing named-pool validator after percent decoding;
reject encoded path separators and native path prefixes. Resolve a hostname once
per client and keep the selected endpoint for that socket's lifetime. Never
change hosts or endpoints while sending a stream.

| Operation | UDP behavior |
| --- | --- |
| `feed` | Send complete messages, using existing input formats and tag rules. |
| `follow` | Receive new messages; existing `--tail` / `--since` select history first. |
| `follow --no-follow` | Attempt the selected finite history, then end; delivery remains best effort. |
| `follow --tag`, `--where`, `--one` | Apply existing client-side filters and output rules. |
| `follow --timeout` | Keep the existing no-matching-output deadline and exit 124. |
| `duplex` | Use the same send and follow operations on two sockets. |
| `fetch`, pool administration, `tap`, access-key commands | Reject a UDP URL with Usage before doing work. |

Existing input decoding handles JSON, YAML, framed Lite3, and raw Lite3. Each
normalized complete message becomes one datagram; input stream framing never
enters the datagram body. JSON/YAML output and framed Lite3 output follow the
existing encoders. Raw Lite3 output remains restricted to `fetch`.

### Sending does not produce a storage receipt

UDP `feed` writes no message envelope or fabricated sequence/time to stdout. Use
the existing Notice encoder with kind `udp-send`, once per invocation; plain
output shows the following text and JSON output encodes the same facts. Print it
even for nonterminal stderr: scripts need the send-success boundary too. Do not
print per-message notices:

```text
UDP sends do not confirm storage; messages may be lost, duplicated, or reordered.
Maximum UDP datagram size: 65507 bytes, including routing and application framing.
```

Exit 0 means every input message validated locally and the operating system
accepted each complete datagram. It does not mean the server received or stored
anything. A later error leaves earlier sends uncertain; never resend them.

Reject `--create`, `--create-size`, `--retry` greater than zero,
`--retry-delay`, and `--durability flush`. Fast remains the default and the
server's append choice. Output-format selection does not turn a send into a
receipt; a UDP feed produces no stdout in every output mode. The input path must
not call the existing append-and-receipt closure with a synthetic Message.

### Following gives an ordered best-effort view

The server sends in pool sequence order. The client exposes only sequences
greater than its last exposed sequence. It discards duplicates and late
arrivals; it does not buffer and reorder data or ask for missing messages. This
preserves normal `follow` output order at the cost of discarding late packets.

A sequence jump produces a stderr notice and continues. Call it a skipped range,
not a proven count of network losses: late arrivals, retention, the server's
outgoing size limit, and send-pressure drops can produce gaps too. Track
incoming sequence before applying filters so filtered messages do not look like
loss.

`--no-follow` ends on the server's terminal progress record. Exit 0 means the
server attempted the selected range and the client observed no gap; it does not
prove complete delivery. A known delivery skip/gap makes a finite read exit 1. A
confirmed core retention gap ends a finite read with RetentionGap (exit 9). Live
reads emit structured `udp-gap` notices and continue. `--quiet-drops` suppresses
those notices without changing accounting or the exit result. A lost terminal
record or unavailable server ends with Io after the transport liveness deadline.
Repeating terminal progress while the lease lives makes a single dropped
terminal datagram survivable without retransmitting messages.

UDP followers reject `--create`, `--replay`, and `--no-notify`, as remote
followers already do. Honor `--quiet-drops`. `duplex` emits received messages,
not send receipts; after stdin ends it follows the existing duplex shutdown
rule.

## Public API: preserve the meaning of append

`RemoteClient` remains the HTTP/HTTPS client. Its append methods return
committed results, and its streams retain their existing contracts. Do not hide
UDP behind those methods or add optional receipt fields to the shared Message
type.

Add Rust `UdpClient`, `UdpPool`, `UdpFollow`, `UdpFollowOptions`, and `UdpEvent`
under `plasmite::api`. Keep protocol structures private. The implemented
API uses these calls:

```rust
let client = UdpClient::new("udp://127.0.0.1:9701")?;
let pool = client.open_pool(&PoolRef::name("sensors"))?;
pool.send_json(&data, &tags)?;           // Result<(), Error>
pool.send_lite3(&document)?;             // Result<(), Error>
let mut stream = pool.follow(UdpFollowOptions::new())?;
while let Some(event) = stream.next_event()? {
    // Message/frame, skipped range, or progress; never a send receipt.
}
```

`open_pool` validates a reference locally; it does not claim the pool exists. A
follow handshake checks existence. Both send methods validate the shared message
contract and the exact encoded datagram size before sending. A connected UDP
socket may report an OS error; it cannot establish successful storage. A client
accepts Name or a UDP Uri on its own endpoint, never Path or a URI for another
endpoint. The constructor rejects pool paths.

Follow options name a start position (`New`, `Last(n)`, `Since(timestamp)`, or
`Sequence(seq)`) and whether to keep following. Default is New and live. Resolve
Last and Since on the server against the opening bounds. For Since, scan
retained timestamps using the existing core cursor and select the first
qualifying sequence; this is new shared selection work, not an existing
timestamp index. Scan before opening PROGRESS, outside the socket receive loop,
bounded by the opening retained range and five-second startup deadline. Hold a
tail-capacity slot during selection; duplicate SUBSCRIBE packets share that
work. Cancel an unfinished scan on expiry or close. Recheck retention before
starting. If no retained frame qualifies, start after the opening upper bound.
Do not send the entire old ring and filter it on the client. After this start
selection, the shared client still checks each timestamp against Since after
sequence accounting: stored timestamps need not increase with sequence. Avoid
reusing TailOptions fields whose meaning depends on local polling, semaphore
notification, retained-gap recovery, or blocking timeout. CLI `--timeout` stays
a matching-output deadline; SDK callers may separately set their wait deadline.

Events contain an owned Lite3 frame with sequence/time, a skipped range and its
reason when known, or progress with the server's next sequence and optional
terminal bound. Shared helpers decode a frame into Message. Keep the binary
attachment intact and aligned, and use existing message validation before
exposing data. The shared client applies the same increasing-sequence rule and
emits a gap event before a higher frame when necessary. Progress can expose a
lost final DATA packet even when the pool turns quiet. The receiver rejects
unknown protocol versions, malformed fields, invalid payloads, and packets from
another endpoint or subscription.

`next_event` drives renewal and receives packets. A caller must keep consuming
(or use a documented receive deadline) to keep its subscription alive. No hidden
thread renews a stream while its consumer has stopped. Drop sends one
best-effort close and releases the socket; expiry handles a lost close. A follow
that loses its server fails; it never silently starts a new stream or mixes
restarted pools.

Deliver the same capability in C, Python, Node, and Go. C adds separate opaque UDP
client/pool/follow handles and send/receive/close functions with the existing
ownership and error conventions. Python and Node expose separate UDP clients
with send methods and follow iteration. Go adds a separate UDP package over
the C functions, keeping its local package local. Preserve full 64-bit sequence
and timestamp values in every binding (BigInt in Node). Reuse the Rust implementation through
native bindings; keep existing local and HTTP client meanings intact. Do not
make users implement the wire protocol to use a supported SDK. Document any
binding/package dependency change before implementation.

## Wire protocol and compatibility

Keep the existing append packet exactly:

```text
UTF-8 pool name | NUL | JSON or Lite3 message body
```

The first body byte still selects Lite3 (`0x06`) or JSON. Bare senders keep
working and receive no acknowledgement. An old server rejects a subscription
request as an empty pool name. A new client times out with a hint that the
server may support UDP input only; it never falls back to HTTP.

New subscription packets start with NUL, which valid legacy pool names cannot
start with. Their common header is 24 bytes:

| Bytes | Meaning |
| --- | --- |
| 0 | NUL: new protocol discriminator |
| 1–4 | ASCII `PUDP` |
| 5 | Version 1 |
| 6 | Operation |
| 7 | Zero padding for header alignment; require zero |
| 8–23 | Cryptographically random 128-bit client subscription ID |

Client operations: SUBSCRIBE, CLOSE. Server operations: CHALLENGE, DATA, SKIP,
PROGRESS, ERROR. SUBSCRIBE with a start selection creates a stream; without one
it renews an existing stream. PROGRESS serves as the opening response and the
renewal response. Assign exact opcode numbers in the normative spec before
implementation. No request IDs beyond the subscription ID, write ACKs, message
ACKs, repair requests, or generic RPC envelope.

Control bodies are UTF-8 JSON objects with strictly bounded, typed fields and no
unknown fields. Cap control packets at 4096 bytes. Pad initial SUBSCRIBE
requests with JSON whitespace to at least 256 bytes. Small responses before
address verification never exceed the received request length. Use the same
strict pool-name rules as append packets.

DATA carries the common header, an eight-byte sequence, an eight-byte timestamp
in nanoseconds, then the complete original Lite3 attachment. Integers use
network byte order. The Lite3 body starts at byte 40, preserving four-byte
alignment when the receive allocation starts aligned. It needs no length prefix:
a UDP datagram already supplies its length.

The maximum remains **65,507 bytes for a complete datagram**, including all
application headers but excluding IP/UDP headers. DATA therefore carries at most
65,467 bytes of Lite3 attachment. Legacy input has a different overhead: pool
name length plus one byte. State that difference in the wire spec.

New client send helpers also require the prepared Lite3 attachment to fit DATA.
`send_json` prepares Lite3 with the existing encoder and sends those bytes;
ordinary raw senders may still use legacy JSON packets. Check both the DATA size
and the routed append size before sending. This prevents our own clients from
storing a UDP-sent message that UDP follow cannot deliver. Long names may lower
the available append-body space further; errors name the actual encoded size.
The one public datagram maximum stays the same. Do not fragment/reassemble
messages in the application. A stored message that cannot fit in DATA produces
SKIP with its sequence and `oversized`, advances the cursor, and never truncates
or blocks other messages. Keep one user-facing maximum; document exact framing
overhead in the protocol contract. Pool messages may exceed this limit through
other transports, so followers must handle skips.

## Starting and ending a subscription

Use UDP for both control and data; require no HTTP access, guessed HTTP port,
access-key exchange, or second client network connection. The server already
runs HTTP/HTTPS, but a UDP sender/follower should need only the UDP endpoint.
Tiny socket clients can implement the same protocol without an HTTP/TLS stack.
This is the reason to accept the small subscription protocol.

An HTTPS-controlled UDP stream would reuse authentication and revocation and
reduce UDP control work. It would also require discovering/configuring a second
endpoint and would give `udp://.../pool` a hidden HTTP dependency. That belongs
in a separate authenticated transport design if we need it.

1. The client binds an ephemeral port, connects its UDP socket to the explicit
   server endpoint, chooses a fresh subscription ID, and sends SUBSCRIBE with
   pool and start selection.
2. The server returns CHALLENGE with a short-lived cookie. Allocate no follower
   and open no pool yet. Authenticate the cookie with a server-process random
   secret over source address/port, subscription ID, canonical typed selection,
   and expiry. A cookie proves return reachability, not identity or permission.
3. The client repeats SUBSCRIBE with that cookie. After verifying it, the server
   reserves a tail-capacity slot and records this subscription before opening
   the pool or scanning history. Duplicate requests share that preparation. It
   fixes the starting selection and finite history upper bound, then sends
   PROGRESS with the initial sequence, next attempted sequence, bound, and lease
   duration. It can then send DATA. Every PROGRESS preserves the original start
   sequence; preparation failures release the slot.
4. The client accepts the first valid DATA or PROGRESS packet for its endpoint
   and subscription ID as proof that the stream started. If opening PROGRESS is
   lost, DATA can still arrive; never discard it simply because control reordered.
   Repeat the identical verified SUBSCRIBE until a valid response arrives.
   Duplicate creation requests resend PROGRESS without allocating another
   follower or resetting its cursor. No DATA retransmission.
5. Every ten seconds the client sends SUBSCRIBE without a start selection.
   The server extends the thirty-second lease and replies with PROGRESS. Any
   valid response refreshes client transport liveness. CLOSE ends a subscription;
   expiry removes it when a client disappears. A renewal never creates a missing
   subscription.

Retries apply only to control, at one-second intervals for at most five seconds
when starting. Later renewal requests continue at their normal cadence. A
thirty-second silence fails the stream with Io, even on a quiet pool. No
per-DATA retry or automatic reconnect. Cookie validity is ten seconds on the
server's monotonic clock; the client merely echoes it. Malformed or unverified
requests receive no error details. Verified errors reuse stable ErrorKind names.
If reads are disabled, only a verified follow request receives Permission; no
unverified request starts work or receives detailed errors.

Each live subscription belongs to one source endpoint, subscription ID, and
opened pool instance. Only that tuple can renew or close it. A conflicting reuse
of the same ID/endpoint with another selection produces Usage after
verification. A server restart invalidates cookies and subscriptions. Pool
deletion/replacement ends the old stream with NotFound, using one shared core
read-only identity check; current checks run on append, so implementation must
expose the same native-file comparison for an open reader and invoke it before
sends and on idle renewal; it never follows a replacement with reset sequence
numbers. Cookie refresh cannot resurrect an expired stream; starting again
requires a fresh ID.

Opening PROGRESS precedes DATA in server send order, but arrival may reorder.
When DATA arrives first, expose it immediately and retain its first sequence.
Later PROGRESS supplies the original start sequence; report any missed initial
range once. Never move the last exposed sequence backward. Server PROGRESS names
the next sequence already attempted, not the pool's current newest sequence. A
progress record beyond the current receive position reports the skipped range
and advances that position; subsequently delayed DATA in that range stays late.
Ignore stale progress so it cannot rewind or repeat notices. Retention skips
must appear in progress/events too. On finite completion, retain a small
terminal record until close/expiry and repeat it on renewal; no retained message
buffers.

## Work bounds and network boundary

UDP remains unauthenticated and unencrypted. Enabling `--udp-follow` makes reads
available; existing input-only listeners never acquire that capability from an
executable update. Anyone who can reach the socket can append to served pools
and, after proving return reachability, read them. This expands the current UDP
exposure to disclosure as well as writes. Saved HTTPS keys and revocation do not
govern this listener. Startup/help must say that explicitly. Use loopback or a
separately protected, controlled network.

Reject broadcast and multicast endpoints in clients and server configuration.
The challenge prevents blind spoofing from turning follow into a reflector; it
does not authorize a subscriber or protect against an on-path attacker.

Use the existing `--max-tail-concurrency` setting (default 64) for UDP
admission. Each current HTTP/HTTPS listener already has its own allowance; give
UDP its own allowance with the same configured value and leave the existing
limits unchanged. No new capacity knob or process-wide semaphore refactor. Return Busy for a verified request at capacity. Before verification, keep
no per-client state. Require valid cookies before expensive work. Bound control
parsing and challenge/error replies. Keep terminal records within the allowance.

Keep one in-flight packet per follower, never an unbounded message queue. If a
nonblocking send cannot proceed, advance the cursor and report the skipped range
in later progress. A slow follower cannot block append input or another
follower. Yield between bounded batches and reserve scheduling turns for control
and input.

Do not invent fixed byte-rate budgets before measurement. Measure byte rate,
fanout, CPU, input latency, and useful delivered messages on the intended LAN
workloads. Add pacing only if those results justify it; keep it internal unless
an operator needs to choose a policy. Capacity and queue bounds control server
resources; they do not supply Internet congestion control. This first version
supports controlled networks only, consistent with RFC 8085 section 3.6.

Read the pool through existing core cursor/tail operations. Filter on the client
so server progress and sequence gaps remain meaningful. Keep start selection,
retention, message validation, and file identity in shared code; do not
implement a second ring reader. An interface worker may hold an ordinary
pool/tail handle for its lease, as HTTP tails already do.

The server's existing lifecycle owns bind/readiness, subscription cancellation,
and shutdown. Bind before readiness. On shutdown stop admission, cancel
followers, finish any active append according to the existing rule, release
sockets, and join workers. Installed services keep the same UDP configuration
across updates.

## Implementation boundaries

- Add private shared UDP packet and client code under `src/api`; export only
  the small UDP API. Keep networking out of `src/core`.
- Extend the current server UDP adapter for control and outgoing tails. The
  current receive loop waits for each blocking append: split reception from
  append execution using one bounded input worker, and run egress independently
  on the shared socket. Saturation discards whole incoming packets, never queues
  them without bound. Keep the legacy append behavior and validators.
  Share encoding constants with the client.
- Add a UDP arm to the CLI pool target and dispatch existing message commands.
  Unsupported commands give a Usage hint naming feed/follow/duplex; update the
  current HTTP-only scheme hint.
  Share input/output processing without inventing an all-transports client trait
  or a generic capability framework.
- Add native binding handles and wrappers, with the same sender/receiver contract.
- Before implementation, add `spec/udp/v1/SPEC.md` for the new controls, leaving
  the legacy v0 input contract intact. Extend CLI and public API specs,
  the C header, help, serving/security guidance, and cookbook. Keep the old UDP
  input proposal as history. This proposal changes no implemented contract by itself.

## Proof and completion

Start with one end-to-end loop: server, pool, CLI UDP follower, CLI UDP sender,
and local fetch proving what the pool stored. Include a handshake test proving
that an arbitrary spoofed subscribe request receives no bulk data.

Use a small UDP proxy in the test harness to deterministically drop, duplicate,
delay, and reorder selected packets. Test actual processes and SDK clients; do
not substitute codec-only unit tests for delivery behavior.

Required proofs:

- CLI and each SDK can send and follow JSON-origin and exact-byte Lite3 messages;
  all existing output/input formats retain their contract.
- Feed emits no receipt, never retries uncertain sends, enforces the final size,
  and distinguishes local validation/OS errors from unknown remote storage.
- Default-directory routing, multiple pools, IPv4/IPv6, hostname selection,
  invalid URLs, unsupported operations/options, and old-server timeout.
- Challenge/address binding, forged/expired cookies, duplicate subscribe and lost
  opening PROGRESS, DATA-before-PROGRESS, lease renew/lost close, quiet pool
  health, configured capacity, and restart.
- Sequence gaps, duplicates/late arrivals, loss of SKIP/PROGRESS, an oversized
  stored message, retention overrun, finite-history completion/lost terminal,
  filters, output timeout, slow stdout, and duplex shutdown.
- A replaced/deleted pool terminates its old subscription. Shutdown while workers
  are active finishes without leaked ports, worker threads, or service state.
- Bounded work under append/follow load; no follower blocks input. Measure bytes,
  useful delivery, and append latency across fanout levels.
- Native Linux/macOS/Windows coverage plus the existing ARM SDK smoke and
  installed-service checks. Update package tests if bindings add runtime files.

Then run required formatting, lint, `just check`, and `just release-gate`.
Measure send-start-to-follower latency and useful throughput against HTTP on
matched payloads, rates, and fanout. Test controlled loss separately. Keep OS
send success separate from committed receipts; a pool timestamp does not mark
the instant of commit. Report
percentiles, received counts, and failures separately; sending more dropped
messages is not more useful throughput. Retain host/setup and three-run raw
evidence. Do not promise a speedup without those measurements.

Completion means the CLI and all named SDKs support both directions, impairment
proofs pass, and the specs describe what users can rely on. An input listener
alone no longer counts as full UDP transport support.

## Review and implementation

Opus-5 reviewed the first draft read-only from this repository with code access.
The revised proposal incorporates its corrections: explicit opt-in for reads,
true input/output size arithmetic, `--quiet-drops`, structured notices, a new UDP
v1 spec, server-side timestamp selection, and bounded independent receive work.
It collapses opening/renewal into SUBSCRIBE/PROGRESS, accepts DATA before opening
PROGRESS, and removes unmeasured rate budgets. A separate code audit verified
receipt, binding, and file-identity constraints.

The design retains pure UDP control because a UDP URL should require one
endpoint and no hidden HTTP/TLS dependency. It retains best-effort finite
history with explicit weaker delivery semantics; it does not claim the
completion guarantee of an HTTP history read. Body-size checks in our clients
prevent sending an attachment that DATA cannot carry, while legacy raw input
keeps its existing acceptance rule.

The implementation fixes opcode numbers and typed control fields in UDP v1.
The server holds progress inside its cursor worker, so a renewal response must
pass that worker's file-identity check. The CLI uses the Rust URL parser. Native
bindings share the Rust client through the C interface and preserve 64-bit
metadata. Node opens follow asynchronously and polls without blocking its event
loop; closing the pool during opening leaves the bounded handshake alive until
it finishes or times out. C callers cancel and join active receives before
freeing a follow handle.

A Sol code review led to the shared parser, worker-owned renewal responses,
strict fixed progress bounds, and Node's nonblocking receive/startup behavior.
The [three-run measurements](../performance/udp-transport.md) caught macOS's
small default send buffer; the client and server now configure both socket
buffers to fit a maximum datagram. Actual server tests and a socket probe
preserve all bytes through an exactly 65,507-byte outgoing datagram and reject
one byte over that limit. The measured workload supports no speed claim.

## Sources

- [Current UDP input contract](../../spec/udp/v0/SPEC.md).
- [Current CLI contract](../../spec/v0/SPEC.md) and
  [public API contract](../../spec/api/v0/SPEC.md).
- [UDP usage guidelines, RFC 8085](https://www.rfc-editor.org/rfc/rfc8085.html):
  message size, return reachability, amplification, and congestion limits.
