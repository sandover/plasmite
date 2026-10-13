# Plasmite UDP Transport Spec (v1)

This contract adds best-effort UDP following and native send/follow clients to
the unchanged [v0 append format](../v0/SPEC.md). UDP provides no acknowledgement
of stored writes, retransmission of messages, authentication, or encryption.
Use loopback or a separately protected controlled network.

## Enablement and targets

`serve --udp` retains append-only behavior. `--udp-follow` explicitly enables
reads and input, using the saved/configured UDP address or 127.0.0.1:9701.
Installed settings default follow to false; omitted options preserve settings.
`serve install --no-udp-follow` withdraws reads; `--no-udp` removes both.

Feed/follow/duplex accept `udp://HOST:PORT/POOL`. Port is explicit. Decode one
pool-name segment and apply ordinary named-pool validation. Reject credentials,
queries, fragments, encoded/native separators, and arbitrary paths. Resolve a
host once, preferring IPv4 when both address families exist to match the default
listener; accept no broadcast/multicast endpoint. Keep that endpoint for every
send and follow from the client. Other commands
reject this scheme with Usage.

A send returns only local validation/OS submission success. Feed emits no
receipt or stdout. It rejects create, retry, and flush options. New clients send
prepared Lite3 bytes and verify both routed append size and follow DATA size.
The whole datagram maximum remains 65,507 bytes, including application framing
and excluding IP/UDP headers. No application fragmentation or reassembly.

## Common header

New control/data packets start with NUL, which cannot start a valid v0 pool
name. Old servers reject them without appending anything.

| Byte offsets | Field |
| --- | --- |
| 0 | NUL |
| 1–4 | ASCII PUDP |
| 5 | Version 1 |
| 6 | Opcode |
| 7 | Zero alignment padding |
| 8–23 | Random 128-bit client subscription ID |

| Opcode | Direction / body |
| --- | --- |
| 1 SUBSCRIBE | Client: typed JSON request |
| 2 CLOSE | Client: no body |
| 3 CHALLENGE | Server: cookie JSON |
| 4 PROGRESS | Server: stream bounds/status JSON |
| 5 SKIP | Server: skipped range JSON |
| 6 ERROR | Server: stable error kind/message JSON |
| 7 DATA | Server: u64 sequence, u64 timestamp ns, complete Lite3 attachment |

DATA integers use big-endian encoding. Its payload starts at offset 40 and is
at most 65,467 bytes. Receive buffers must provide four-byte alignment. Datagram
length supplies payload length. Validate received message shape and nested data
before exposing it. Reject unknown versions/opcodes, nonzero padding, malformed
fields, unexpected source endpoints, and unknown subscription IDs.

Control packets never exceed 4096 bytes and contain one JSON object, with no
unknown/duplicate fields. Initial SUBSCRIBE packets include trailing JSON
whitespace to make their total length at least 256 bytes. Before verifying
return reachability, responses never exceed the triggering request length.

## Control objects

SUBSCRIBE: optional `selection` and `cookie`. An absent selection means renew,
not create. Selection contains `pool` (validated name), `start`, and `live`.
Start has a `kind` of `new`, `last`, `since`, or `sequence`; kinds other than new
require unsigned u64 `value` (message count, timestamp ns, or sequence).
Cookie contains `expires_ms` (server monotonic time) and `mac` (base64url,
without padding, HMAC-SHA256). CHALLENGE contains `cookie`.

PROGRESS contains unsigned `start_seq`, `next_seq`, optional `end_seq`,
`lease_ms`, boolean `terminal`, and boolean `retention_gap`. The start and
finite upper bound never change. Next sequence names records already attempted,
not merely the current pool newest sequence. SKIP contains inclusive unsigned
`first`, `last`, and `reason` (`oversized`, `retention`, or `pressure`). ERROR
contains stable `kind` and `message`; disclose no local paths or secrets.

## Subscription lifecycle

A fresh client socket and random ID send SUBSCRIBE with a selection. The server
issues a cookie before opening a pool or allocating per-client state. It signs
source IP/port, ID, expiry, and canonical typed selection with a random process
secret. Cookies last ten seconds. The client echoes the selection and cookie.
Only valid cookies permit pool access or tail admission. Disabled reads return
Permission only after this proof. Cookies prove reachability, never identity.

Admission uses the configured max-tail-concurrency (default 64) separately from
existing HTTP listener allowances. Preparation occupies one slot and checks a
five-second deadline. Duplicate creation requests with identical selection
share preparation or receive existing progress; they never restart a cursor.
Conflicting selection returns Usage. Verified capacity failure returns Busy.

Start New selects after the opening newest sequence. Last chooses up to N
retained records. Since scans the opening retained range for the first matching
timestamp; no match selects after the upper bound. Sequence begins at the
specified sequence, with retained-gap accounting. Empty pools begin at 1.
Finite streams fix the opening upper bound; live streams keep following.
A core cursor performs reads/retention checks. Preparation must not block the
UDP receive loop. Timestamp filtering continues client-side after accounting
for sequence; timestamps need not increase with sequence.

Opening PROGRESS precedes DATA in server send order, but arrivals may reorder.
The client accepts DATA before PROGRESS and keeps the first received sequence;
later progress can disclose an initially missed range. Only initial controls
retry (one-second spacing, five-second deadline). Writes/DATA never retry.

Renew every ten seconds using SUBSCRIBE without selection. Only an existing
matching endpoint/ID extends a thirty-second lease. The server returns progress
even for a quiet pool or completed finite stream. Client silence for thirty
seconds ends with Io. Drop/CLOSE sends one best-effort stop; lost close expires.
No automatic reconnect or reuse of IDs across starts. Restart invalidates all
subscriptions. Pool deletion/replacement ends with NotFound; no stream may
silently follow a replacement with reused sequence numbers. Check native file
identity before sends and on idle renewal.

Completed finite streams retain only their terminal progress until close/expiry.
A dropped terminal packet can therefore survive normal renewal without DATA
replay. No per-message acknowledgement or confirmation of complete delivery.

## Receive ordering and notices

Expose strictly increasing sequences; discard duplicate/late DATA. Account for
sequence before filters. A jump emits a skipped-range event, not a claim about
confirmed network loss. Progress can expose a lost final message; accept only
advancing progress and never rewind. Oversized stored messages produce SKIP and
advance whole; never truncate them. Finite delivery gaps exit 1; confirmed core
retention gaps exit 9. Live gaps continue with a structured udp-gap Notice;
quiet-drops suppresses notices without suppressing accounting.

Follow timeout retains its no-matching-output meaning (exit 124). Finite exit 0
means the server attempted the selected range and no gap appeared, not proof of
complete delivery. A lost terminal record/unavailable peer eventually produces
Io. Unsupported remote follow options retain Usage errors.

## Resource and shutdown rules

One bounded append worker keeps legacy storage behavior independent of control
and egress. Saturation drops whole packets. Each follower has one in-flight
packet and no unbounded queue. WouldBlock advances with pressure accounting.
Bound unverified challenge responses globally and keep no challenge table.
Follow workers yield between bounded batches; neither egress nor history
selection blocks reception. Capacity/queue limits do not replace Internet
congestion control; this version supports controlled networks only.

Server readiness follows all successful binds. Shutdown stops admission,
cancels followers, finishes any active append, joins workers, and releases
sockets. Saved HTTPS keys and revocation do not govern UDP.
