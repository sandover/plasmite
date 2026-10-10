# UDP input for Plasmite pools

Draft design, October 9, 2026. The commands and protocol below describe proposed
behavior.

Add an optional User Datagram Protocol (UDP) listener to `plasmite serve`. It
serves the same pool directory as the existing HTTP and HTTPS listeners: `--dir`
selects that directory, and omitting it uses `~/.plasmite/pools`. Each datagram
names an existing pool in that directory and carries one message. Use this input
for sensor readings, heartbeats, and other small messages whose senders accept
loss, duplication, and reordering. HTTP keeps the complete remote API.

## Message and delivery model

The sender chooses the destination pool and the message contents. Server
configuration chooses the directory and the socket. The receiver
validates each complete datagram, resolves its pool name in the served
directory, and calls the existing
[append operation](../../spec/api/v0/SPEC.md#required-operation-semantics). The
pool assigns the sequence number and append time, commits the message, and
exposes it to ordinary readers.

```text
sender -> datagram naming a pool -> resolve in served directory -> append -> readers
```

UDP supplies the message boundary, and the datagram's leading pool name supplies
routing. Plasmite adds no sender registration, acknowledgement, retransmission,
or duplicate suppression. The receiver never sends a UDP reply, not even for
invalid input.

The contract has these limits:

- The network can lose, duplicate, or reorder datagrams. A duplicate that
  reaches the append becomes another pool message.
- A successful socket send means the sender's operating system accepted the
  datagram. It says nothing about reception or storage.
- The receiver accepts at most **65,507 bytes of UDP payload**, including the
  pool name, its NUL delimiter, and the message body. It discards a larger
  datagram whole.
- Invalid input, storage pressure, and append errors can all prevent an append.
  The sender gets no error and no receipt. The receiver never retries an append,
  because some I/O errors happen after publication.
- Sequence order and retention keep their current rules. A packet lost before
  the append leaves no gap in pool sequence numbers. A sender that must detect
  missing or stale readings puts its own counter or sample time in `data`.
- Appends use the existing `Durability::Fast` policy. They do not flush to
  stable storage before the append completes.

### One size limit

65,507 bytes is the conventional IPv4 UDP payload maximum behind a 20-byte IP
header; IPv6's conventional maximum is 65,527. The smaller number gives senders
one limit across both address families, and it applies to both encodings and
every listener address. Help text and startup output state this number. They
offer no second recommendation and no tuning option.
[UDP size limits](https://datatracker.ietf.org/doc/html/rfc8085#section-1)

A smaller arbitrary cap would reject ordinary messages, because Lite3 documents
cost much more than their JSON equivalent at these sizes. Measured with the
current encoder, including the eight-byte `sensors` routing prefix: a two-field
temperature reading with one tag is 69 bytes as a JSON datagram and 469 bytes as
Lite3; an object with `samples` holding integers 0 through 255 and one tag is
962 and 10,777. These constructed examples illustrate encoding overhead;
they do not describe production traffic. Lite3 forwards
prepared binary messages; it does not promise compression. A 4 KiB cap would
reject the samples example. Smaller arbitrary caps have no measured receiver
requirement behind them.

The limit bounds parser input. Decoded values and the append encoding allocate
more, and it bounds no traffic rate, so the controlled-network and bounded-rate
requirements below still hold. Encoded message contents that exceed the pool's
frame capacity fail inside the append with the existing `Usage` error. JSON
encoding into Lite3 can expand the contents, so a datagram within the UDP limit
can still exceed a small pool's capacity. The existing storage rules apply.

Plasmite never splits a message or reassembles one. The operating system can
reassemble IP fragments before delivering a datagram, and acceptance does not
reveal whether that happened. Fragmentation raises the chance of loss, and the
delivery contract above applies at every accepted size. Send larger messages
through HTTP.
[UDP message-size guidance](https://datatracker.ietf.org/doc/html/rfc8085#section-3.2)

## Server composition

One `serve` process owns the existing HTTP and HTTPS listeners and the optional
UDP listener, all over the selected pool directory. Senders can reach several
pools through one UDP socket, then inspect those pools through HTTP, the
browser, or local readers.

The directory therefore has one service configuration and one lifecycle. Each
listener keeps its own decoding and access rules and then calls the same pool
operations, so the two inputs share no transport protocol and negotiate nothing.
A configured listener that fails to bind prevents startup, and a fatal listener
failure ends the process instead of leaving a half-working service.

Separate directories or deployment lifecycles still call for separate processes;
UDP input does not. An exclusive HTTP-or-UDP mode would complicate discovery and
take HTTP inspection away from UDP workflows, so this first version keeps
ordinary serving on and makes UDP input optional.

## Command line

Add this option to the shared `serve` and `serve install` arguments:

| Option | Meaning |
| --- | --- |
| `--udp[=ADDRESS]` | Enable UDP input; omitted address means `127.0.0.1:9701`. |

Add `--no-udp` to `serve install` itself to remove saved UDP settings. It is an
installation action, not part of the shared or serialized runtime arguments.

Bare `--udp` selects the default address, and `--udp=ADDRESS` selects another.
Requiring `=` for an explicit address keeps this option from swallowing the
positional HTTPS `SERVER` argument. Clap expresses this with an optional value,
`default_missing_value`, and `require_equals`.

`ServeRunArgs` stores one optional UDP address: `Some(address)` enables input and
`None` disables it. Validate the address in `serve_service::effective_args`
after the existing merge step handles updates. Omitting `--udp` keeps saved
settings; supplying bare `--udp` selects the default address. An omitted
`--dir` keeps the normal default-directory behavior.

Pool names resolve through `resolve_named_pool_path` on a `LocalClient`. Tighten
that shared resolver, which today rejects only path separators, to also reject
empty names, NUL bytes, and platform path prefixes such as the Windows
drive-relative `C:sensors`. Every named-pool caller inherits the checks, and no
name can then select another directory.

Addresses use the same numeric `IP:PORT` notation as `--remote-bind`, with
brackets around IPv6 addresses. A foreground listener accepts port zero and
prints the port the operating system assigns; an installed listener requires a
fixed nonzero port. A wildcard bind follows the existing server convention and
prints the all-interface warning. The listener takes numeric addresses only and
joins no multicast group; a multicast bind address is a `Usage` error.

```console
plasmite pool create sensors events
plasmite serve --udp
```

The default accepts local traffic only. A non-loopback address requires an
explicit `--udp=ADDRESS`; the server never infers one from the HTTP or HTTPS
listener. To select another directory, use the existing global option:

```console
plasmite --dir ./shared serve --udp
```

`SERVER`, `--bind`, and `--remote-bind` keep their current meanings, and UDP runs
alongside those listeners. One process runs one UDP listener, which serves every
pool in its directory.

For a protected network, name its concrete interface address:

```console
plasmite serve --udp=100.100.10.20:9701
```

One listener accepts both JSON and prepared Lite3 messages. Their first body
bytes distinguish them, as defined below; this needs no format setting.
`--max-body-bytes` still governs HTTP bodies only. UDP uses the fixed 65,507-byte
limit and adds no size option.

### Installed server lifecycle

The existing lifecycle owns the UDP listener:

```console
plasmite serve install --udp
plasmite serve logs --follow
plasmite serve install --no-udp
```

`install` merges supplied UDP options into saved settings and then validates the
whole configuration. An omitted option keeps its saved value. A first
enablement needs only `--udp`; an existing setup changes its address with
`--udp=ADDRESS`. `--no-udp` conflicts with `--udp` and clears the address. It lives
on `ServeSubcommand::Install` beside that command's `json` flag, and the install
path passes it into the settings merge as a separate action, so saved runtime
settings never carry a removal instruction. An install that disables UDP starts
the ordinary HTTP and HTTPS server with the remaining settings.

The new runtime field deserializes with a default, so older saved setups still
load. It skips serialization when absent, which keeps `setup.run` output
unchanged while UDP is off; comment that deliberate difference from the older
fields, which serialize as `null`. Unix `Setup::argv` must emit `--udp=ADDRESS`
as one token so an installed service receives its saved setting;
Windows starts from the saved manifest instead. Check that saved options
round-trip through Unix argument generation.

`start`, `stop`, `restart`, and `uninstall` keep their current meanings and
platform support. Restarts and executable updates preserve UDP settings, and
older saved setups omit them and keep UDP off. The Unix installer's port
preflight must test a UDP socket alongside its existing TCP sockets, skipping the
UDP probe only when the saved UDP address has not changed and the running service
owns that listener. The Windows backend has no equivalent TCP preflight, and this
feature adds none. Binding at startup stays authoritative on every platform. TCP
and UDP can use the same port number.

### Startup and status output

Move the serving banner out of `secure_serve::run` into the preparation stage,
after directory setup and after every configured listener binds. Print the
addresses actually bound, including operating-system-assigned ports, and then
publish readiness. With UDP enabled, add these lines to stderr:

```text
UDP input: 127.0.0.1:9701 (JSON/Lite3, served pool directory)
UDP delivery may lose, duplicate, or reorder messages.
Maximum UDP message size: 65507 bytes including routing. Larger messages are discarded.
Sending does not confirm storage. UDP has no authentication or encryption.
```

Command help states the same contract. A non-loopback bind adds:

```text
Allow only trusted senders through a protected network.
```

A wildcard bind also prints the existing all-interface warning.

The `serve status` JSON shape stays as it is, identifying the server process and
its HTTP addresses. `serve status --all --json` already exposes saved serve
options under `setup.run`, so installed setups show their UDP configuration
there. Startup output and `serve logs` carry listener details and receive
summaries. UDP input does not appear in the browser's HTTP activity panel. This
feature adds no status command and no HTTP endpoint.

## Datagram API

Applications send through an ordinary UDP socket. The destination address selects
the server, and each datagram names an existing pool in the served directory,
playing the same role as the pool component of an HTTP URL. A datagram cannot
select a directory or request a read, creation, deletion, or tail.

Both formats use the same routing prefix:

```text
[UTF-8 pool name][0x00][message body]
```

The first NUL byte ends the name. Filesystem names cannot contain NUL, so the
delimiter needs no escaping and no length field. Reject a missing delimiter, an
empty or invalid-UTF-8 name, an invalid pool name, and an empty body. Resolve the
name through `PoolRef::name` and the shared resolver, never as a general pool
reference. Everything after the delimiter is message body. The 65,507-byte limit
covers prefix and body together. Routing bytes never enter stored message
contents.

Select the decoder from the first body byte: `0x06` selects Lite3; any other
byte goes to the JSON parser. Then validate the complete body with that decoder.
Never retry invalid input with the other decoder. Empty bodies are invalid.

This rule follows the current append contract. The vendored Lite3 format uses
a leading type byte; `LITE3_TYPE_OBJECT` is 6 in `vendor/lite3/include/lite3.h`.
`Lite3DocRef::validate` requires an object root, so every valid Plasmite Lite3
message starts with `0x06`. JSON cannot start with that control byte. State this
rule in the UDP specification and guard it when updating Lite3. It requires no
format flag, negotiation, or added wire field.

### JSON

For JSON, the body is one UTF-8 JSON object carrying the
existing HTTP append fields `data` and optional `tags`:

```json
{"data":{"temperature_c":22.4,"sample":81},"tags":["sensor"]}
```

The existing append requires object-valued `data`, and the UDP adapter inherits
that check. `tags` defaults to an empty list and otherwise must be an array of
strings. The parser consumes the whole document and allows ordinary JSON
whitespace. It rejects missing `data`, duplicate request fields, unknown top-level
fields, trailing content, invalid UTF-8, and malformed JSON. An empty datagram is
invalid. Datagram boundaries replace the line boundaries of JSON Lines, so two
documents in one datagram are invalid.

The body matches HTTP append input, except that the UDP parser also rejects
unknown fields, which the current HTTP parser ignores. A small concrete request
struct with Serde's `deny_unknown_fields` supplies that check, and Serde's
ordinary struct decoder already rejects duplicate request fields.

The input has no `seq`, `time`, or `durability` field; applications put their own
identifiers and sample times inside `data`. The receiver calls
`append_json_now(data, tags, Durability::Fast)`, which builds the message content
and lets the pool assign metadata. That call also builds a receipt holding a
clone of `data`, which UDP drops. Accept that bounded cost rather than adding a
second append path to avoid it.

This sender uses only Python's standard library:

```python
import json
import socket

message = {"data": {"temperature_c": 22.4, "sample": 81},
           "tags": ["sensor"]}
body = json.dumps(message, separators=(",", ":")).encode("utf-8")
payload = b"sensors\x00" + body
if len(payload) > 65_507:
    raise ValueError("UDP payload exceeds 65507 bytes; use HTTP")
with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
    sock.sendto(payload, ("127.0.0.1", 9701))
```

The operator reads stored messages through the existing CLI:

```console
plasmite follow sensors --tail 1
```

### Lite3

For Lite3, the body is the complete Lite3 message document that
`append_lite3_now` accepts, and the sender forms the datagram as
`b"sensors\x00" + message_bytes`. The receiver splits at the first NUL only; the
Lite3 body may contain NUL freely. `append_lite3_now` validates the document,
preserves its bytes, and builds no receipt.

The content document holds `meta.tags` and object-valued `data`. The pool assigns
destination sequence and append time outside those content bytes.

The routing prefix leaves stored content unmodified. There is no HTTP header, no
source sequence, and no timestamp prefix. An HTTP `tail_lite3` frame carries
extra stream metadata, so a relay must extract the Lite3 payload before sending
it as a datagram. Binary encoding does not guarantee a smaller document, so a
relay must measure the complete datagram.

### Public library API

This feature adds no public Rust type, C function, binding method, or UDP pool
reference. Socket libraries already send datagrams and the pool API already
appends messages. `feed`, `follow`, `fetch`, `duplex`, MCP, saved connections,
and access keys keep their current targets and contracts. UDP configuration
belongs to the server's interface layer.

## Network access boundary

UDP input lets anyone who can reach its socket append to any existing pool in the
served directory, including pools created after startup. The loopback default
matches the existing local HTTP access scope. On a non-loopback interface, UDP
grants that directory-wide append access without the access keys HTTPS requires,
and it supplies no sender identity, encryption, or revocation. Serve a separate
directory, using the existing `--dir` boundary, when access must cover a smaller
set of pools.

The operator controls the directory contents. Pool opens follow symbolic links as
they do today, so directory selection is not a filesystem sandbox. Local
processes can use a loopback listener; a non-loopback listener needs a separately
protected network whose own rules limit senders. Use an encrypted tunnel when
messages need confidentiality. A concrete bind address limits which interface
listens; it establishes no trust by itself.

Never put a `pk1` access key or its secret in a datagram. Those credentials
depend on TLS server verification, and UDP uses neither that key scheme nor
browser sessions. Revoking an HTTPS key does nothing to UDP reachability.

This first version supports controlled networks and modest, bounded sender rates.
The operator keeps aggregate traffic within network and receiver capacity; the
server's memory bounds are not congestion control. General Internet exposure
falls outside this contract. [UDP usage guidelines, sections 3.1 and
3.6](https://www.rfc-editor.org/rfc/rfc8085.html#section-3.6)

## Receiver implementation

Keep socket reception, message decoding, and pool append as separate steps in one
small interface module, `src/udp_input.rs`. Reuse `LocalClient`, pool name
validation, and the existing append calls. Leave `src/core`, pool files, sequence
allocation, and the public append contract alone, as the existing
[transport architecture](../record/architecture.md#transport-architecture)
requires.

Resolve CLI settings into one internal `Option<SocketAddr>`. A value enables
the listener and `None` disables it, so runtime code never meets a
half-configured input; partial
installation updates stay in the argument-merge step. Pass this value to
`serve_secure_pair` separately, and keep UDP settings out of `ServeConfig`, which
the server clones for its two HTTP listeners.

Use one receive loop with one append in flight:

1. Receive one datagram into a reusable 65,536-byte buffer. Ordinary UDP
   datagrams fit this buffer on every supported platform.
2. Discard any result longer than 65,507 bytes before parsing. The buffer size is
   not the protocol's limit.
3. Extract and validate the pool name, then select the decoder from the first
   body byte. Parse JSON here; the existing Lite3 append validates binary input.
4. Through `spawn_blocking`, open `PoolRef::name(pool)` on a `LocalClient` for the
   served directory and append. Drop the handle when the attempt finishes, and
   await completion before receiving again. This needs no pool registry, cache, or
   application queue.
5. Record the outcome and reuse the buffer.

The buffer covers ordinary IPv4 and IPv6 sizes; IPv6 jumbograms extend those
limits and fall outside this protocol. A truncated receive that fills the buffer
still exceeds 65,507 bytes and so cannot reach parsing, which holds as long as the
limit stays below the buffer size.
[IPv6 jumbograms](https://www.rfc-editor.org/rfc/rfc2675.html#section-4)

The operating system's socket buffer holds datagrams while an append runs, and
drops them when it fills. A competing writer that holds the pool's file lock
stalls this loop, and the same socket-buffer limit then applies. The receiver
holds at most one outstanding blocking append. It takes no HTTP storage-executor
permit, changes no HTTP admission limit, and adds no concurrency option.

Opening a pool per datagram follows the HTTP server's directory model: messages
can reach pools created after startup, and no cached handle outlives a deletion or
replacement. Opening costs something, so add a bounded cache only if measurement
later shows a need. This design has none.

Validate configuration and bind UDP before registering the server as ready. An
empty served directory is valid. An unavailable socket fails startup with the
existing error kind and a concrete hint. A missing or unreadable destination is a
message error, and the loop continues. Deletion or replacement during an append
follows the existing file-handle semantics, and the next message resolves its
destination anew. The receiver never creates a pool in response to a datagram.

Use `tokio::select!` over reception, summary ticks, and shutdown while idle, and
over append completion, summary ticks, and shutdown while an append runs. A
shutdown request stops reception and awaits the running append rather than
dropping its join handle; pool locks can delay that completion, and this design
adds no append cancellation. The receiver then closes the socket without draining
pending datagrams.

An interrupted receive resumes the loop. Any other receive error or task failure
reports an error and requests the ordinary shutdown of all listeners. The
coordinator keeps and awaits their futures so HTTP uses its existing shutdown
paths, then returns the failure to the supervisor's restart policy. Adding UDP to
the current `try_join!` would instead drop the HTTP futures on an error and skip
those paths. One shared shutdown notification coordinates the listeners; it
carries no client state.

### Receive summaries

Keep four counters for the process: `received`, `appended`, `oversized`, and
`errors`. Emit a summary through `tracing::info!` every 30 seconds when the
counters change, and once at orderly shutdown. The existing log filter controls
verbosity, and `serve logs` captures the summaries. Avoid per-message logs,
payload logging, sender tracking, and persisted metrics.

`received` counts successful socket receives, and each received datagram lands in
exactly one terminal counter. No summary can count network or operating-system
drops before reception. `errors` covers parse, validation, pool-open, and append
errors together. Some append errors leave the storage outcome uncertain, so the
counter must not be described as messages that failed to reach storage. Grouping
them also avoids duplicating Lite3 validation just to separate malformed input
from storage corruption in a counter.

## Validation and contract updates

The first useful proof sends JSON messages to two pools through one loopback
listener and reads them with ordinary `follow`. Repeat with prepared Lite3
documents and check that stored bytes exclude the routing prefix and preserve the
original bodies exactly.

Focused checks must cover:

- Messages at 65,507 bytes, and oversized messages from 65,508 through 65,527
  bytes over IPv6, which permits those payload sizes. IPv4 senders cannot send an
  ordinary datagram above 65,507, so check that sender-side error separately. An
  oversized datagram whose prefix forms valid JSON or Lite3 must never append.
  Check reception on Unix and native Windows, rejection of a full receive buffer
  before parsing, and the documented sender length guard.
- Empty, malformed, and invalid-shape messages; empty and
  invalid-UTF-8 wire names; and missing routing delimiters. Verify no sequence
  advance and correct counters. Test empty, NUL-containing, separator-containing,
  and platform-prefixed names directly against the shared resolver. Verify that
  NUL bytes in the body reach the selected decoder: JSON rejects a literal NUL,
  and Lite3 may contain one.
  Check mixed JSON and Lite3 traffic on one listener, JSON with leading
  whitespace, rejection of Lite3 array roots, malformed bodies beginning with
  `0x06`, and no fallback to another decoder. Guard the object-root type byte.
- Two pools through one socket, pools created after startup, and unknown
  destinations. Verify routing and the reject paths, including Windows
  drive-relative names, that could otherwise override directory selection.
- Duplicate datagrams, several senders, and a sender whose own sample counter
  arrives out of order. Verify pool append order and preserved sender data without
  asserting any network delivery guarantee.
- Writer contention, ambiguous append errors, a message larger than the
  destination ring, pool deletion or replacement, socket failure, and shutdown
  both while idle and with an append outstanding. Verify bounded work, name
  resolution after a destination is replaced, normal HTTP shutdown after a fatal
  UDP failure, and no append retries or UDP replies.
- Default-directory and `--dir` selection, UDP enablement, name
  and address validation, an occupied UDP port, saved-setup merge and removal, and
  restart persistence. Verify that servers with UDP disabled keep their current
  behavior and status JSON. Check bare `--udp`, explicit `--udp=ADDRESS`, and
  rejection of a space-separated UDP address, and confirm that bare `--udp` cannot
  consume the positional `SERVER`. Extend the existing help-output tests in
  `tests/cli_help_output.rs` to the new flags.

Before implementation, add a short `spec/udp/v0/SPEC.md` for the datagram
contract; the remote specification keeps governing HTTP. Add the CLI options and
lifecycle rules to `spec/v0/SPEC.md`. Update `docs/cli.md`, `docs/cookbook.md`,
and `docs/record/serving.md` with the workflow and the access boundary. Clarify
the architecture document's HTTPS requirement to separate authenticated remote
pool control from this explicit UDP append input. Leave the public library API
contract and the on-disk format unchanged. Add the UDP spec to the spec index and
the AGENTS docs map once that file exists, and follow the existing testing policy
and required checks when implementation begins.
