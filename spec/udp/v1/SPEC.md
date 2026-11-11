# Plasmite UDP Transport Spec (v1)

This contract adds best-effort UDP following and native send/follow clients to
the unchanged [v0 append format](../v0/SPEC.md). UDP provides no acknowledgement
of stored writes, retransmission of messages, authentication, or encryption.
Use loopback or a separately protected controlled network.

**Source status (2026-11-11):** Clients implement the arrival-order policy below.
This unreleased change replaces 1.x late/duplicate suppression and inferred gaps.
It keeps the packet format; shipping the changed client behavior requires a
major release under the product's
[compatibility policy](../../../docs/record/releasing.md#versioning-policy).

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
UDP receive loop. Timestamp filtering continues client-side on each received
message; timestamps need not increase with sequence.

Opening PROGRESS precedes DATA in server send order, but arrivals may reorder.
The client accepts DATA before PROGRESS. Later progress describes server attempts
and never establishes which messages the client received. Only initial controls
retry (one-second spacing, five-second deadline). Writes/DATA never retry.

Renew every ten seconds using SUBSCRIBE without selection. Only an existing
matching endpoint/ID extends a thirty-second lease. The server returns progress
even for a quiet pool or completed finite stream. Client silence for thirty
seconds ends with Io. Drop/CLOSE sends one best-effort stop; lost close expires.
No automatic reconnect or reuse of IDs across starts. Restart invalidates all
subscriptions. Pool deletion/replacement ends with NotFound; no stream may
silently follow a replacement with reused sequence numbers. Check native file
identity before sends and on idle renewal.

Completed finite streams retain terminal progress and the original pool identity
until close/expiry. Completion releases the pool mapping and reader worker.
A dropped terminal packet can therefore survive normal renewal without DATA
replay. No per-message acknowledgement or confirmation of complete delivery.

## Receive ordering and notices

- Deliver each valid DATA packet received for the active subscription immediately
  in arrival order, subject to the requested selection and filters.
- Deliver late messages and duplicates. Sequence numbers identify messages;
  they never act as a receive cutoff. Arrivals `10, 12, 11, 12` produce
  `10, 12, 11, 12` when all four messages match the filters.
- Do not buffer for reordering, suppress duplicates, retransmit DATA, or fetch
  missing messages through another transport.
- A sequence jump or advancing PROGRESS does not establish loss. Neither
  produces an inferred gap notice or prevents later DATA delivery.
- Report explicit server skips. A SKIP or PROGRESS record never makes a
  later-arriving DATA packet ineligible. Oversized messages produce SKIP;
  the server never truncates them. Filters never create skip notices.
- Keep subscription setup, renewal, expiry, and close behavior.
- A finite follow ends on the first valid terminal PROGRESS and drains events
  it already accepted. DATA still in flight may go unread; completion gives
  no assurance of complete delivery. Add no grace period or repair exchange.

Explicit oversized/pressure skips produce structured `udp-gap` notices and
finite exit 1. Confirmed server retention gaps, including `retention_gap` in
PROGRESS, produce finite exit 9. If only the retention flag arrives, report
retention without inventing a sequence range. Live follows report server skips
and continue.
`--quiet-drops` suppresses notices without changing the exit result. A finite
follow with terminal progress and no observed server skip/error exits 0, even
if the network lost DATA or SKIP packets. Follow timeout retains its
no-matching-output meaning (exit 124). An unavailable peer eventually produces
Io. Unsupported remote follow options retain Usage errors.

Public `Gap` event names and the CLI notice kind `udp-gap` are compatibility
names for explicit server skips. The public/ABI `Unknown` reason remains reserved;
SKIP packets must name `oversized`, `retention`, or `pressure`.

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
