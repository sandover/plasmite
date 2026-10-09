# UDP input for Plasmite pools

Draft design, October 9, 2026. The commands and protocol below describe proposed
behavior.

Add an optional User Datagram Protocol (UDP) listener to `plasmite serve`.
Each datagram carries one complete message into one existing local pool.
Applications use this input for sensor readings, heartbeats, and other small
messages when they accept loss, duplication, and changes in arrival order.
HTTP continues to provide the complete remote API.

## Message and delivery model

The sender chooses message contents. Server configuration chooses the socket,
destination pool, and encoding. The receiver validates each complete datagram
and invokes the existing
[append operation](../../spec/api/v0/SPEC.md#required-operation-semantics). The
pool assigns its sequence number and append time, commits the message, and
exposes it through ordinary readers.

```text
sender -> UDP datagram -> validate -> append to configured pool -> existing readers
```

UDP supplies message boundaries. Plasmite adds no framing header, routing field,
sender registration, acknowledgement, retransmission, or duplicate suppression.
The receiver never sends a UDP response, including for invalid messages.

The contract has these limits:

- Network delivery can lose, duplicate, or reorder datagrams. A duplicate that
  reaches the append operation becomes another pool message.
- A successful socket send means the sender's operating system accepted the
  datagram. It says nothing about reception or storage.
- The receiver accepts at most **1,200 bytes of UDP payload**, including the
  entire JSON or Lite3 document. It discards larger datagrams in full.
- Invalid messages, storage pressure, and append errors can prevent an append.
  The sender receives no error or receipt. The receiver never retries an append:
  some I/O errors can occur after publication.
- Pool readers retain the existing sequence-order and retention rules. A packet
  lost before append leaves no pool sequence gap. A sender that needs to detect
  missing or stale readings can put its own counter or sample time in `data`.
- UDP appends use the existing `Durability::Fast` policy. They do not request a
  flush to stable storage before completing the append operation.

The fixed size limit bounds parsing work and reduces the need for IP
fragmentation on common network paths. A tunnel or smaller maximum transmission
unit (MTU) can require smaller messages; 1,200 bytes does not guarantee delivery
or avoid fragmentation on every path. There is no application fragmentation or
reassembly. Send larger messages through HTTP. [UDP usage guidelines, sections
3.2 and 3.3](https://www.rfc-editor.org/rfc/rfc8085.html#section-3.2)

## Server composition

One `serve` process owns the existing HTTP and HTTPS listeners and the optional
UDP listener. Give each listener an explicit address and use separate port
numbers in examples. Users can send readings through UDP, then inspect or follow
the same pool through HTTP, the browser, or local readers.

This arrangement gives the directory one service configuration and lifecycle.
Each listener keeps its own decoding and access rules, then calls the same pool
operations. Supporting both inputs requires no shared transport protocol or
negotiation. A failed configured listener prevents successful startup; a fatal
listener failure ends the process rather than leaving a partially working
service.

Separate server processes serve separate directories or deployment lifecycles
when users need that isolation. The UDP feature does not require a second
process. Exclusive HTTP-or-UDP modes would complicate discovery and remove HTTP
inspection from UDP workflows, so this first version keeps ordinary serving
enabled and makes UDP input optional.

## Command line

Add these options to the shared `serve` and `serve install` arguments:

| Option | Meaning |
| --- | --- |
| `--udp-bind ADDRESS` | Numeric IP address and port for UDP input. |
| `--udp-pool NAME` | Existing pool in the directory selected by `--dir`. |
| `--udp-format json\|lite3` | Datagram encoding; default `json`. |

Add `--no-udp` to `serve install` itself to remove saved UDP settings. It is an
installation action, not part of the shared or serialized runtime arguments.

After argument merging, `--udp-bind` and `--udp-pool` must both be present or
both absent. A format without an enabled listener produces `Usage`. Apply this
rule in `serve_service::effective_args`, where existing option-pair validation
lives. Do not add clap `requires` rules to the shared arguments: an installation
update can supply one field and retain the other from saved settings.

Pool names use `resolve_named_pool_path` through `LocalClient`, rejecting every
platform path separator. Paths cannot select another directory.
Addresses use the same numeric `IP:PORT` notation as `--remote-bind`, with
brackets around IPv6 addresses. Foreground listeners accept port zero and print
the port the OS assigns. Installed listeners require a fixed nonzero port.
Wildcard binds follow the existing server convention and print an all-interface
warning. The listener accepts numeric addresses only and joins no multicast
groups; a multicast bind address produces `Usage`.

```console
plasmite --dir ./pools pool create sensors
plasmite --dir ./pools serve --udp-bind 127.0.0.1:9701 --udp-pool sensors
```

Port 9701 is an example, not a default. An explicit socket makes the network
boundary visible and avoids inferring it from the HTTP or HTTPS listener.
`SERVER`, `--bind`, and `--remote-bind` retain their current meanings. UDP runs
alongside those listeners. The process supports one UDP listener and one
destination pool.

For a protected network, select its concrete interface address:

```console
plasmite --dir ./pools serve --udp-bind 100.100.10.20:9701 --udp-pool sensors
```

To accept prepared binary messages, add `--udp-format lite3`. The configured
format applies to every datagram; the listener never guesses an encoding.
`--max-body-bytes` continues to govern HTTP bodies. UDP's fixed 1,200-byte limit
has no tuning option in this first version.

### Installed server lifecycle

The existing lifecycle owns the UDP listener:

```console
plasmite --dir ./pools serve install --udp-bind 127.0.0.1:9701 --udp-pool sensors
plasmite --dir ./pools serve logs --follow
plasmite --dir ./pools serve install --no-udp
```

`install` merges supplied UDP options with saved settings, then validates the
complete configuration. Omitted options retain saved values, including the
format. A first enablement needs both bind and pool; an existing UDP setup can
change either field alone. `--no-udp` conflicts with all UDP options and clears
the bind, pool, and format together. It applies only to `serve install`; the
flag lives on `ServeSubcommand::Install` beside its `json` flag, and the install
path passes that action separately into the settings merge. Saved runtime
settings contain no removal instruction. An install that disables
UDP starts the ordinary HTTP and HTTPS server with the remaining settings.

`start`, `stop`, `restart`, and `uninstall` retain their existing meanings and
platform support. Restarts and executable updates preserve UDP settings. Older
saved setups omit them and keep UDP disabled. Port preflight checks must test a
UDP socket as well as the existing TCP sockets in the Unix installer. When
reinstalling a running service, skip the UDP probe only if the saved UDP bind
has not changed and that service owns the listener. The Windows backend has no
equivalent TCP preflight; this feature adds none. Final startup binding remains
authoritative on every platform. TCP and UDP can use the same numeric port.

### Startup and status output

Move the existing serving banner from `secure_serve::run` to the preparation
stage after all configured listeners bind and the destination pool opens.
Print actual bound addresses, including OS-assigned ports, then publish
readiness. With UDP enabled, add these lines to stderr:

```text
UDP input: 127.0.0.1:9701 -> sensors (json)
UDP delivery may lose, duplicate, or reorder messages.
Maximum UDP message size: 1200 bytes. Larger messages are discarded.
Sending does not confirm storage. UDP has no authentication or encryption.
```

Command help states the same contract. A non-loopback bind adds:

```text
Allow only trusted senders through a protected network.
```

A wildcard bind also uses the existing all-interface warning convention.

The existing `serve status` JSON shape stays unchanged. It identifies the server
process and its HTTP addresses. `serve status --all --json` already exposes
saved serve options under `setup.run`; installed setups include their UDP
configuration there. Startup output and `serve logs` show listener details and
receive summaries. UDP input does not appear in the browser's HTTP request
activity panel. This feature adds no status command or HTTP endpoint.

## Datagram API

Applications send directly through an ordinary UDP socket. The destination
address selects the receiver, whose configuration selects the pool. A datagram
cannot name another pool or request a read, creation, deletion, or tail.

### JSON

One datagram contains one UTF-8 JSON object with the existing HTTP append fields
`data` and optional `tags`:

```json
{"data":{"temperature_c":22.4,"sample":81},"tags":["sensor"]}
```

The existing append implementation requires object-valued `data`; the UDP
adapter inherits that check. `tags` defaults to an empty list and otherwise must
be an array of strings. The parser consumes the complete document, allowing normal
JSON whitespace. It rejects missing `data`, duplicate request fields, unknown
top-level fields, trailing content, invalid UTF-8, and malformed JSON. An empty datagram
is invalid. Datagram boundaries replace the line boundaries of JSON Lines;
multiple documents in one datagram are invalid.

The field names match HTTP append input. The UDP parser also rejects unknown
fields, which the current HTTP parser ignores. A small concrete request struct
with Serde's `deny_unknown_fields` supplies that check; Serde's ordinary struct
decoder already rejects duplicate request fields.

The input has no `seq`, `time`, `pool`, or `durability` field. Applications put
their own identifiers and sample times inside `data`. The receiver calls
`append_json_now(data, tags, Durability::Fast)`, which constructs the existing
message content and assigns the destination pool metadata. That API also builds
a receipt containing a clone of `data`; UDP discards the receipt. Accept this
bounded cost for messages of at most 1,200 bytes rather than adding another
append path solely to avoid it.

This sender uses only Python's standard library:

```python
import json
import socket

message = {"data": {"temperature_c": 22.4, "sample": 81}, "tags": ["sensor"]}
payload = json.dumps(message, separators=(",", ":")).encode("utf-8")
if len(payload) > 1200:
    raise ValueError("UDP message exceeds 1200 bytes")
with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
    sock.sendto(payload, ("127.0.0.1", 9701))
```

The operator reads stored messages through the existing CLI:

```console
plasmite --dir ./pools follow sensors --tail 1
```

### Lite3

With `--udp-format lite3`, a datagram contains the same complete Lite3 message
document accepted by `append_lite3`: `meta.tags` and object-valued `data`.
The existing append API validates the document and preserves its bytes. The
pool assigns destination sequence and append time outside those content bytes.

There is no HTTP header, stream length prefix, source pool sequence, or timestamp
prefix. In particular, an HTTP `tail_lite3` frame contains extra stream metadata;
a relay must extract the Lite3 payload before sending it as a datagram.

The same 1,200-byte limit applies to encoded Lite3 bytes. Binary encoding does
not guarantee a smaller document. Relays must measure the complete payload.

### Public library API

This feature adds no public Rust type, C function, binding method, or UDP pool
reference. Existing socket libraries already send datagrams, and the existing
pool API already appends messages. `feed`, `follow`, `fetch`, `duplex`, MCP,
saved connections, and access keys retain their current targets and contracts.
UDP configuration belongs to the server's interface layer.

## Network access boundary

UDP input grants anyone who can reach its socket permission to append to the
configured pool. It provides no sender identity, encryption, or revocation.
Local processes can use a loopback listener; a non-loopback listener requires a
separately protected network whose access rules limit senders. Use an encrypted
tunnel when messages need confidentiality. A concrete bind address limits the
listening interface; it does not establish trust by itself.

Never put a `pk1` access key or its secret in a datagram. Those credentials rely
on TLS server verification. UDP uses neither that key scheme nor browser
sessions. Revoking an HTTPS key has no effect on UDP reachability.

The first version supports controlled networks and modest, bounded sender rates.
The operator must keep aggregate traffic within network and receiver capacity.
The server's memory bounds do not provide network congestion control. General
Internet exposure falls outside this contract. [UDP usage guidelines, sections
3.1 and 3.6](https://www.rfc-editor.org/rfc/rfc8085.html#section-3.6)

## Receiver implementation

Keep socket reception, message decoding, and pool append as separate steps in a
small interface module, `src/udp_input.rs`. Reuse `LocalClient`, pool name
validation, and the existing append API. Do not change `src/core`, pool
files, sequence allocation, or the public append contract. This follows the
existing [transport architecture](../record/architecture.md#transport-architecture).

Resolve CLI settings into one internal `Option<UdpInputConfig>` containing a
typed socket address, pool name, and encoding enum. A complete value enables the
listener; `None` disables it. Keep partial installation updates in the existing
argument-merge step, so runtime code never has to resolve half-configured input.
Pass this value separately to `serve_secure_pair`. Keep UDP settings out of
`ServeConfig`, which the server clones for its two HTTP listeners.

Use one receive loop with one append attempt at a time:

1. Receive one datagram into a reusable 65,536-byte buffer. Conventional UDP
   datagrams fit in this buffer on every supported platform.
2. Discard any result longer than 1,200 bytes before parsing. The receive-buffer
   size does not change the protocol's acceptance limit.
3. Decode JSON when configured. For Lite3, pass the bounded bytes to the existing
   validated append API. The configured format determines the append operation.
4. Perform the append through `spawn_blocking` using the pool handle opened at
   startup. Move the handle into the closure and return it with the append
   result. Await completion before receiving another datagram; this requires
   neither an additional mutex nor an application queue.
5. Record the outcome, then reuse the receive buffer.

The buffer covers ordinary IPv4 and IPv6 UDP sizes. IPv6 jumbograms extend those
limits and fall outside this input protocol. A truncated successful receive
that fills the buffer still exceeds 1,200 bytes and cannot reach parsing.
[IPv6 jumbograms](https://www.rfc-editor.org/rfc/rfc2675.html#section-4)

The OS socket buffer holds datagrams while an append runs. If it fills, the OS
drops packets. A competing writer can block the pool's file lock and stall this
loop; the same socket-buffer limit applies. The receiver has at most one
outstanding blocking append. It uses no HTTP storage-executor permits, changes
no HTTP admission limits, and adds no worker-concurrency option.

Open the selected pool once at startup and retain that handle, as `feed` does.
Validate configuration and bind UDP before registering the server as ready. A
missing pool or unavailable socket fails startup with the existing error kind
and a concrete hint. If deletion or replacement invalidates the handle, its
`NotFound` result ends serving; a restart opens the current named pool. The
receiver neither creates pools nor silently switches to a replacement.

Use `tokio::select!` to handle reception, summary ticks, and shutdown while idle.
While an append runs, select its completion, summary ticks, and shutdown. A
shutdown request stops further reception and awaits the existing append rather
than dropping its join handle. Pool locks can delay completion; there is no new
append-cancellation mechanism. The receiver closes the socket without draining
pending datagrams.

An interrupted receive resumes the loop. Other receive errors, task failures,
or an invalidated destination report an error and request the ordinary shutdown
of all listeners. The coordinator retains and awaits their futures so HTTP can
use its existing shutdown paths, then returns the failure for the supervisor's
restart policy. Simply adding UDP to the current `try_join!` would drop HTTP
futures on an error and skip their shutdown paths. Use one shared shutdown
notification to coordinate these existing listeners; it carries no client state.

### Receive summaries

Keep four counters for the current process: `received`, `appended`, `oversized`,
and `errors`. Emit a summary with `tracing::info!` every 30 seconds when counters
change, and at orderly shutdown. The existing log filter controls verbosity;
`serve logs` captures these summaries.
Avoid per-message logs, payload logging, sender tracking, and persisted metrics.

`received` counts successful socket receives. Each consumed datagram gets one
terminal outcome counter. Summaries
cannot count network or OS drops before reception. `errors` includes parse,
validation, and append errors. Some append errors leave storage
outcome uncertain, so it must not claim that all such messages failed to reach
storage. Grouping these errors avoids duplicating Lite3 validation merely to
distinguish malformed input from storage corruption in a counter.

## Why this remains simple

Delivery and storage have separate contracts. A datagram carries content;
configuration supplies routing; the pool owns ordering and persistence. The
receiver never tries to infer sender intent or restore network history.

One listener, one configured format, and one pool eliminate routing tables and
format negotiation. A fixed limit and a serial worker eliminate tuning surfaces
and application queues. Existing append functions own message validation and
storage correctness. Small concrete functions suffice; no transport trait,
adapter registry, shared session framework, or reliable-UDP protocol serves
this workflow.

## Validation and contract updates

The first useful proof runs the documented JSON sender against a loopback
listener and reads the resulting message through ordinary `follow`. Repeat
with a prepared Lite3 document and verify exact stored payload bytes.

Focused checks must cover:

- Messages at 1,200 bytes and oversized messages at 1,201 bytes and well above
  the acceptance limit, including maximum conventional UDP payloads. An
  oversized datagram whose prefix forms valid JSON or Lite3 must never append.
  Check this receive behavior on Unix and native Windows.
- Empty, malformed, wrong-format, and invalid-shape messages. Verify no sequence
  advance and correct outcome counts.
- Duplicate datagrams, multiple senders, and a sender that emits its own sample
  counter out of order. Verify pool append order and preserve sender data without
  asserting network delivery guarantees.
- Writer contention, ambiguous append errors, pool deletion or replacement,
  socket failure, and shutdown both while idle and with an outstanding append.
  Verify bounded work, restart after destination replacement, normal HTTP
  shutdown on a fatal UDP failure, and no append retries or UDP replies.
- Bind/pool option pairs, format selection, name and address validation, occupied
  UDP ports, saved-setup merge/removal, and service restart persistence. Verify
  that servers with UDP disabled preserve existing behavior and status JSON.
  Update the command-inventory and locked option-surface tests for new flags.

Before implementation, add a short `spec/udp/v0/SPEC.md` for the UDP datagram
contract. The existing remote specification continues to govern HTTP. Add the
CLI options and lifecycle rules to `spec/v0/SPEC.md`. Update `docs/cli.md`,
`docs/cookbook.md`, and `docs/record/serving.md` with the workflow and access
boundary. Clarify the architecture's HTTPS requirement to distinguish
authenticated remote pool control from this explicit UDP append input. Keep the
public library API contract and on-disk format unchanged. Add the UDP spec to
the spec index and AGENTS docs map when that file exists. Follow the existing
testing policy and required checks when implementation begins.
