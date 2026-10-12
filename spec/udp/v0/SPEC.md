# Plasmite UDP Input Spec (v0)

This document defines the UDP input contract for Plasmite 1.x. It covers
append-only datagrams. The HTTP API keeps its separate contract.

## Listener

`plasmite serve --udp` enables one UDP listener alongside the server's existing
HTTP and HTTPS listeners. The default address is `127.0.0.1:9701`; an explicit
address uses `--udp=ADDRESS`. Global `--dir` selects the pool directory, with
the usual default when omitted. Addresses must use numeric IPv4 or IPv6 addresses;
multicast addresses are invalid. Foreground listeners may use port zero and print
the bound address. Installed listeners require a fixed nonzero port.
The listener can append to every existing pool
in that directory, including pools created after startup.

`serve install --no-udp` removes a saved UDP listener setting. Omitting `--udp`
from an install update keeps the saved setting; older installations without a
UDP setting keep UDP disabled. UDP settings survive restart and executable
updates. The option adds no public library API or pool format change.

## Datagram format

Each datagram contains one complete message in this form:

```text
[UTF-8 pool name][0x00][message body]
```

The first NUL byte ends the pool name. The name must be nonempty, valid UTF-8,
and contain no NUL, native path separator, or native platform path prefix
(including Windows drive-relative names such as `C:sensors`). Resolve it through
the ordinary named-pool rules, including the optional `.plasmite` suffix.
Reject a missing
delimiter, invalid name, missing pool, or empty body. Routing bytes are not
stored in the message.

The whole datagram, including the name and delimiter, must not exceed 65,507
bytes. Discard larger datagrams whole. Do not split or reassemble application
messages.

The first body byte selects its format. `0x06` selects Lite3; validate the whole
body as one Lite3 object document and preserve its bytes. Every other first byte
selects JSON; validate the whole body as one UTF-8 JSON object with required
object-valued `data` and optional string-array `tags`. Reject duplicate or
unknown top-level fields, malformed or trailing content, and invalid field
types. Never retry invalid input with the other decoder.

Append accepted messages through the existing append behavior with
`Durability::Fast`. The pool assigns sequence and append time. UDP adds no
acknowledgement, reply, authentication, encryption, retransmission, retry, or
duplicate suppression. The network may lose, duplicate, or reorder datagrams;
each duplicate that reaches append creates another message. A successful send
does not confirm reception or storage.

The receiver does not expose reads, pool creation, deletion, or arbitrary path
selection. Senders that need reliable delivery or larger messages must use the
existing HTTP interface.
