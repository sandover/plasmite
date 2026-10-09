# UDP input for Plasmite pools

Draft design, October 9, 2026. The commands and protocol below describe proposed
behavior.

Add an optional User Datagram Protocol (UDP) listener to `plasmite serve`.
The listener serves the same pool directory as HTTP and HTTPS. The existing
`--dir` selects that directory; omitting it uses `~/.plasmite/pools`. Each
datagram names an existing pool in that directory and carries one message.
Applications use this input for sensor readings, heartbeats, and other small
messages when they accept loss, duplication, and changes in arrival order.
HTTP continues to provide the complete remote API.

## Message and delivery model

The sender chooses the destination pool and message contents. Server
configuration chooses the directory, socket, and encoding. The receiver
validates each complete datagram, resolves its pool name in that directory,
and invokes the existing
[append operation](../../spec/api/v0/SPEC.md#required-operation-semantics). The
pool assigns its sequence number and append time, commits the message, and
exposes it through ordinary readers.

```text
sender -> datagram naming a pool -> resolve in served directory -> append -> readers
```

UDP supplies message boundaries. Each datagram includes a pool name for routing.
Plasmite adds no sender registration, acknowledgement, retransmission, or
duplicate suppression.
The receiver never sends a UDP response, including for invalid messages.

The contract has these limits:

- Network delivery can lose, duplicate, or reorder datagrams. A duplicate that
  reaches the append operation becomes another pool message.
- A successful socket send means the sender's operating system accepted the
  datagram. It says nothing about reception or storage.
- The receiver accepts at most **N bytes of UDP payload**, including the pool
  name and encoded message. It discards larger datagrams in full. Choose N
  before freezing the protocol; 1,200 bytes remains a provisional value.
- Invalid messages, storage pressure, and append errors can prevent an append.
  The sender receives no error or receipt. The receiver never retries an append:
  some I/O errors can occur after publication.
- Pool readers retain the existing sequence-order and retention rules. A packet
  lost before append leaves no pool sequence gap. A sender that needs to detect
  missing or stale readings can put its own counter or sample time in `data`.
- UDP appends use the existing `Durability::Fast` policy. They do not request a
  flush to stable storage before completing the append operation.

The acceptance limit bounds parsing work. Choose it after checking representative
JSON and Lite3 messages and the networks this feature supports. A conservative
sending size and the receiver's acceptance limit serve different purposes.
IPv6's minimum packet size leaves 1,232 bytes after its basic IP and UDP headers;
1,200 leaves some header space. That makes 1,200 a useful sending baseline, but
does not establish the right rejection limit for Plasmite. Tunnels and smaller
path limits can still cause fragmentation or loss. [QUIC datagram sizing](https://www.rfc-editor.org/rfc/rfc9000.html#section-14)

The receiver uses one fixed 64 KiB buffer regardless of N. This bounds receive
memory independently of the acceptance policy. There is no application
fragmentation or reassembly. Send messages above N through HTTP.

## Server composition

One `serve` process owns the existing HTTP and HTTPS listeners and the optional
UDP listener. All use the selected pool directory. Users can send readings to
several pools through one UDP socket, then inspect those pools through HTTP,
the browser, or local readers.

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
| `--udp[=ADDRESS]` | Enable UDP input; omitted address means `127.0.0.1:9701`. |
| `--udp-format json\|lite3` | Datagram encoding; default `json`. |

Add `--no-udp` to `serve install` itself to remove saved UDP settings. It is an
installation action, not part of the shared or serialized runtime arguments.

`--udp` supplies the default address; `--udp=ADDRESS` supplies another address.
Require `=` for an explicit address so this option cannot consume the positional
HTTPS `SERVER` argument. Clap can express this with an optional value,
`default_missing_value`, and `require_equals`.

Store one optional UDP address in `ServeRunArgs`: `Some(address)` enables input,
and `None` disables it. After argument merging, a UDP format without an address
produces `Usage` in `serve_service::effective_args`. The existing merge step
handles address and format updates. Omitting `--udp` retains saved settings;
supplying bare `--udp` explicitly selects the default address. An omitted
`--dir` retains the normal default-directory behavior.

Pool names use `resolve_named_pool_path` through `LocalClient`. Tighten that
shared resolver to reject empty names, NUL characters, platform path separators,
and platform path prefixes, including Windows drive-relative names such as
`C:sensors`. All named-pool callers inherit those checks; paths cannot select
another directory.

Addresses use the same numeric `IP:PORT` notation as `--remote-bind`, with
brackets around IPv6 addresses. Foreground listeners accept port zero and print
the port the OS assigns. Installed listeners require a fixed nonzero port.
Wildcard binds follow the existing server convention and print an all-interface
warning. The listener accepts numeric addresses only and joins no multicast
groups; a multicast bind address produces `Usage`.

```console
plasmite pool create sensors events
plasmite serve --udp
```

The default accepts local traffic. A non-loopback address requires an explicit
`--udp=ADDRESS`; never infer it from the HTTP or HTTPS listener. To select another
directory, use the existing global option:

```console
plasmite --dir ./shared serve --udp
```

`SERVER`, `--bind`, and `--remote-bind` retain their current meanings. UDP runs
alongside those listeners. The process supports one UDP listener serving all
existing pools in its directory.

For a protected network, select its concrete interface address:

```console
plasmite serve --udp=100.100.10.20:9701
```

To accept prepared binary messages, add `--udp-format lite3`. The configured
format applies to every datagram; the listener never guesses an encoding.
`--max-body-bytes` continues to govern HTTP bodies. The initial UDP design adds
no size-tuning option; the acceptance limit remains an open protocol decision.

### Installed server lifecycle

The existing lifecycle owns the UDP listener:

```console
plasmite serve install --udp
plasmite serve logs --follow
plasmite serve install --no-udp
```

`install` merges supplied UDP options with saved settings, then validates the
complete configuration. Omitted options retain saved values, including the
format. A first enablement needs only `--udp`; an existing UDP setup can change
its address with `--udp=ADDRESS` or its format with `--udp-format`. `--no-udp`
conflicts with both UDP options and clears address and format together. It
applies only to `serve install`; the
flag lives on `ServeSubcommand::Install` beside its `json` flag, and the install
path passes that action separately into the settings merge. Saved runtime
settings contain no removal instruction. An install that disables
UDP starts the ordinary HTTP and HTTPS server with the remaining settings.

New runtime fields deserialize with defaults so older saved setups still load.
Serialization omits absent UDP fields, preserving `setup.run` output when UDP
is off. Comment this deliberate difference from older fields that serialize as
`null`. Apply the format default after merging; a Clap default must not overwrite
a saved format during a partial update. Unix `Setup::argv` must emit
`--udp=ADDRESS` as one token and the format as `--udp-format VALUE` so an
installed service receives its saved settings. Windows
already starts from the saved manifest. Check that saved options round-trip
through Unix argument generation.

`start`, `stop`, `restart`, and `uninstall` retain their existing meanings and
platform support. Restarts and executable updates preserve UDP settings. Older
saved setups omit them and keep UDP disabled. Port preflight checks must test a
UDP socket as well as the existing TCP sockets in the Unix installer. When
reinstalling a running service, skip the UDP probe only if the saved UDP address
has not changed and that service owns the listener. The Windows backend has no
equivalent TCP preflight; this feature adds none. Final startup binding remains
authoritative on every platform. TCP and UDP can use the same numeric port.

### Startup and status output

Move the existing serving banner from `secure_serve::run` to the preparation
stage after directory setup and all configured listeners bind.
Print actual bound addresses, including OS-assigned ports, then publish
readiness. With UDP enabled, add these lines to stderr:

```text
UDP input: 127.0.0.1:9701 (json, served pool directory)
UDP delivery may lose, duplicate, or reorder messages.
Maximum UDP message size: N bytes. Larger messages are discarded.
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

Applications send directly through an ordinary UDP socket. Its destination
address selects the server; each datagram names an existing pool in the served
directory. The name plays the same role as the pool component of an HTTP URL.
Datagrams cannot select a directory or request a read, creation, deletion, or tail.

Both formats use the same routing prefix:

```text
[UTF-8 pool name][0x00][message body]
```

The first NUL byte ends the name. Filesystem names cannot contain NUL, so the
delimiter needs no escaping or length field. Reject a missing delimiter, empty
or invalid UTF-8 name, invalid pool name, or empty body. Resolve the name through
`PoolRef::name` and the shared named-pool resolver; never parse it as a general
pool reference. Bytes after the delimiter belong to the message body. The
N-byte limit covers the prefix and body together. Routing never enters stored
message contents.

### JSON

With the default `json` format, the body contains one UTF-8 JSON object with the
existing HTTP append fields `data` and optional `tags`:

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

The body matches HTTP append input. The UDP parser also rejects unknown
fields, which the current HTTP parser ignores. A small concrete request struct
with Serde's `deny_unknown_fields` supplies that check; Serde's ordinary struct
decoder already rejects duplicate request fields.

The input has no `seq`, `time`, or `durability` field. Applications put
their own identifiers and sample times inside `data`. The receiver calls
`append_json_now(data, tags, Durability::Fast)`, which constructs the existing
message content and assigns the destination pool metadata. That API also builds
a receipt containing a clone of `data`; UDP discards the receipt. Accept this
bounded cost for messages within N bytes rather than adding another
append path solely to avoid it.

This sender uses only Python's standard library:

```python
import json
import socket

message = {"data": {"temperature_c": 22.4, "sample": 81},
           "tags": ["sensor"]}
body = json.dumps(message, separators=(",", ":")).encode("utf-8")
payload = b"sensors\x00" + body
with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
    sock.sendto(payload, ("127.0.0.1", 9701))
```

Once N is fixed, add a sender-side length check against the complete payload.
The operator reads stored messages through the existing CLI:

```console
plasmite follow sensors --tail 1
```

### Lite3

With `--udp-format lite3`, the body contains the complete Lite3 message document
accepted by `append_lite3`. A sender forms the datagram as
`b"sensors\x00" + message_bytes`. The receiver splits only at the first NUL;
the Lite3 body may contain NUL freely.

The content document contains `meta.tags` and object-valued `data`.
The existing append API validates the document and preserves its bytes. The
pool assigns destination sequence and append time outside those content bytes.

The pool prefix supplies routing without modifying the stored content. There
is no HTTP header, source pool sequence, or timestamp prefix. In particular,
an HTTP `tail_lite3` frame contains extra stream metadata;
a relay must extract the Lite3 payload before sending it as a datagram.

Binary encoding does not guarantee a smaller document. Relays must measure
the complete datagram.

### Public library API

This feature adds no public Rust type, C function, binding method, or UDP pool
reference. Existing socket libraries already send datagrams, and the existing
pool API already appends messages. `feed`, `follow`, `fetch`, `duplex`, MCP,
saved connections, and access keys retain their current targets and contracts.
UDP configuration belongs to the server's interface layer.

## Network access boundary

UDP input grants anyone who can reach its socket permission to append to any
existing pool in the served directory, including pools created after startup.
The loopback default matches the existing local HTTP access scope. On a
non-loopback interface, UDP grants directory-wide append access without the
access keys that HTTPS requires. It provides no sender identity,
encryption, or revocation. Serve a separate directory when access must cover
only a smaller set of pools, using the existing `--dir` boundary.
The operator controls the directory contents. Existing pool opens follow
symbolic links; directory selection does not add a filesystem sandbox.
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
typed socket address and encoding enum. A complete value enables the
listener; `None` disables it. Keep partial installation updates in the existing
argument-merge step, so runtime code never has to resolve half-configured input.
Pass this value separately to `serve_secure_pair`. Keep UDP settings out of
`ServeConfig`, which the server clones for its two HTTP listeners.

Use one receive loop with one append attempt at a time:

1. Receive one datagram into a reusable 65,536-byte buffer. Conventional UDP
   datagrams fit in this buffer on every supported platform.
2. Discard any result longer than N bytes before parsing. The receive-buffer
   size does not change the protocol's acceptance limit.
3. Extract and validate the pool name and decode JSON when configured. The
   configured format determines the append operation.
4. Through `spawn_blocking`, open `PoolRef::name(pool)` on a `LocalClient` for
   the served directory and append through the existing JSON or Lite3 API.
   Drop the handle when the attempt finishes. Await completion before receiving
   another datagram; this requires no pool registry, cache, or application queue.
5. Record the outcome, then reuse the receive buffer.

The buffer covers ordinary IPv4 and IPv6 UDP sizes. IPv6 jumbograms extend those
limits and fall outside this input protocol. A truncated successful receive
that fills the buffer still exceeds N and cannot reach parsing, provided N
stays below the buffer size.
[IPv6 jumbograms](https://www.rfc-editor.org/rfc/rfc2675.html#section-4)

The OS socket buffer holds datagrams while an append runs. If it fills, the OS
drops packets. A competing writer can block the pool's file lock and stall this
loop; the same socket-buffer limit applies. The receiver has at most one
outstanding blocking append. It uses no HTTP storage-executor permits, changes
no HTTP admission limits, and adds no worker-concurrency option.

Opening a pool per datagram follows the HTTP server's directory model. It lets
messages use pools created after startup and avoids stale cached handles after
deletion or replacement. Pool opening has a cost; add a bounded cache only if
measurement later establishes a need. This design includes no cache.

Validate configuration and bind UDP before registering the server as ready.
An empty served directory is valid. An unavailable socket fails startup with
the existing error kind and a concrete hint. A missing or inaccessible
destination counts as a message error; continue receiving. Deletion or
replacement during an append follows the existing file-handle semantics. The
next message resolves its destination anew. The receiver never creates pools
in response to a datagram.

Use `tokio::select!` to handle reception, summary ticks, and shutdown while idle.
While an append runs, select its completion, summary ticks, and shutdown. A
shutdown request stops further reception and awaits the existing append rather
than dropping its join handle. Pool locks can delay completion; there is no new
append-cancellation mechanism. The receiver closes the socket without draining
pending datagrams.

An interrupted receive resumes the loop. Other receive errors or task failures
report an error and request the ordinary shutdown of all listeners. The
coordinator retains and awaits their futures so HTTP can
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
terminal outcome counter. Summaries cannot count network or OS drops before
reception. `errors` includes parse,
validation, pool-open, and append errors. Some append errors leave storage
outcome uncertain, so it must not claim that all such messages failed to reach
storage. Grouping these errors avoids duplicating Lite3 validation merely to
distinguish malformed input from storage corruption in a counter.

## Why this remains simple

Delivery and storage have separate contracts. A datagram names a pool and carries
content; the pool owns ordering and persistence. The
receiver never tries to infer sender intent or restore network history.

One listener serves the same directory as the other interfaces. Ordinary named
pool lookup supplies routing without a routing table. A configured format and
a serial worker avoid negotiation, client state, and application queues.
Existing append functions own message validation and
storage correctness. Small concrete functions suffice; no transport trait,
adapter registry, shared session framework, or reliable-UDP protocol serves
this workflow.

## Validation and contract updates

The first useful proof sends JSON messages to two pools through one loopback
listener and reads them through ordinary `follow`. Repeat with prepared Lite3
documents and verify that stored bytes exclude the routing prefix and preserve
the original message bodies exactly.

Focused checks must cover:

- Messages at N bytes and oversized messages at N+1 bytes and well above
  the acceptance limit, including maximum conventional UDP payloads. An
  oversized datagram whose prefix forms valid JSON or Lite3 must never append.
  Check this receive behavior on Unix and native Windows.
- Empty, malformed, wrong-format, and invalid-shape messages; empty or invalid
  UTF-8 wire names; and missing routing delimiters. Verify no sequence advance
  and correct outcome counts. Test empty, NUL-containing, separator-containing,
  and platform-prefixed names directly at the shared resolver.
  Verify that NUL bytes in the body reach the configured decoder: JSON rejects
  literal NUL, while Lite3 can contain it.
- Two pools served through one socket, pools created after startup, and
  unknown destinations. Verify correct routing and reject paths, including
  Windows drive-relative names, that could override directory selection.
- Duplicate datagrams, multiple senders, and a sender that emits its own sample
  counter out of order. Verify pool append order and preserve sender data without
  asserting network delivery guarantees.
- Writer contention, ambiguous append errors, pool deletion or replacement,
  socket failure, and shutdown both while idle and with an outstanding append.
  Verify bounded work, name resolution after destination replacement, normal HTTP
  shutdown on a fatal UDP failure, and no append retries or UDP replies.
- Default-directory and `--dir` selection, UDP enablement, format selection,
  name and address validation, occupied
  UDP ports, saved-setup merge/removal, and service restart persistence. Verify
  that servers with UDP disabled preserve existing behavior and status JSON.
  Check bare `--udp`, explicit `--udp=ADDRESS`, and rejection of a space-separated
  UDP address. Confirm that bare `--udp` cannot consume positional `SERVER`.
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
