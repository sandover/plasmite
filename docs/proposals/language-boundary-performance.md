# End-to-end performance across language bindings

Plasmite can remove substantial conversion work by carrying Lite3 and native
ownership farther into each binding. The ordinary object APIs also need faster
paths: callers should not have to encode Lite3 themselves to get good performance.
This assessment proposes work; it reports no new benchmark results or completed
optimization. The accompanying Ergo epic holds the individual proposals.
The [Opus 5 review and decisions](language-boundary-performance-review.md) explain
the revisions and the remaining uncertainties.

## Why the conversions exist

The C application binary interface (ABI) accepts and returns JSON bytes. That
choice gives C, Python, Node, and Go one portable interface with simple ownership.
Rust's convenient message API uses `serde_json::Value`, an allocated tree of
objects, arrays, strings, and numbers. The Lite3 encoder accepts JSON text. These
three useful interfaces meet in a path that repeatedly changes representation:

```text
Language object
  -> JSON text/UTF-8 bytes
  -> Rust Value tree
  -> JSON text containing meta and data
  -> yyjson tree
  -> Lite3 bytes
  -> pool commit

Read through the C JSON API
  -> owned Lite3 snapshot
  -> data as JSON text
  -> Rust Value tree
  -> JSON message envelope
  -> language-owned bytes/string
  -> language object
```

Ordinary append also returns a full message. The Rust path clones the input
data for that receipt; the C interface serializes the result, and the binding
decodes it. This work buys a committed sequence, timestamp, and complete message,
but callers who only need confirmation pay for the complete data round trip.
Reusing the caller's mutable object as the receipt would change snapshot behavior.

Several conversions serve a contract: JSON syntax and number handling, readable
nested data, committed receipt identity, stable storage snapshots, and language
memory ownership. Removing an intermediate representation can retain those
contracts. Removing the checks or handing out mutable pool memory cannot.

Sources: [C adapter](../../src/abi.rs), [message API](../../src/api/message.rs),
[Lite3 bridge](../../src/core/lite3/mod.rs),
[vendor decoder](../../vendor/lite3/src/json_dec.c),
[public C contract](../../include/plasmite.h).

## What each runtime changes

| Surface | Current path and useful existing behavior | Proposed direction |
| --- | --- | --- |
| Rust | Object append starts with `Value`; encoding serializes it before yyjson parses it. Message reads export data to JSON and parse it. Raw tails avoid building a data tree, but `Meta::from_lite3` still exports and scans JSON for readability. | Direct Serde encoding/decoding; typed or borrowed reads tied to an owned snapshot; binary readability validation. Keep existing `Message` behavior. |
| C | JSON operations build Rust messages and serialize them. Raw get/tail already return owned Lite3 frames; append of Lite3 already returns only a sequence. The header exposes no public field access API. | Separate byte-oriented JSON read and append bridges, small committed receipts, and coarse field access over existing owned frames. C callers must not need a second Lite3 installation or an internal shim. |
| Python | `ctypes.CDLL` calls the C ABI. Ordinary append uses `json.dumps().encode()`; reads copy native output, decode UTF-8, then call `json.loads`. Immutable local append inputs already borrow bytes during the native call. UDP send still uses `from_buffer_copy`; UDP frames defer message decode. | Finish cheap byte-path improvements, then measure a native extension that walks objects once and exposes owned binary buffers and selected fields. Python dictionaries and strings still require Python allocations. |
| Node | The addon already uses Node-API. Objects go through `JSON.stringify`, Buffer, C JSON, Buffer-to-string, and `JSON.parse`. Receipt parsing runs a reviver across the tree to preserve large envelope sequences. Native output copies ABI buffers into a Rust Vec before handing the Vec to Node. Each awaited local read submits an `AsyncTask`. UDP eagerly decodes both raw bytes and a message. | First test a cheap parser for proven native envelope sequences; transfer ABI buffer ownership to Node and test direct object conversion against V8's fast JSON builtins, and address worker-pool scheduling separately. Keep arbitrary payload numbers distinct from envelope `bigint` sequences. |
| Go | cgo already borrows input byte slices for the call. `Append` marshals values; JSON reads copy C output into Go. `DecodeMessage` keeps `Data` as `json.RawMessage`, so it already avoids a generic data tree, but copies `Raw` again. Tags allocate/free C strings individually. UDP eagerly builds a message as well as copying its payload. | Remove duplicate owned-JSON copies and tag crossings first. Offer coarse selected-field/typed Lite3 reads without cgo calls per field. Keep ordinary Go-owned slices safe to retain. |

Sources: [Python](../../bindings/python/plasmite/__init__.py),
[Node wrapper](../../bindings/node/index.js),
[Node message parsing](../../bindings/node/message.js),
[Node addon](../../bindings/node/native/src/lib.rs),
[Go local](../../bindings/go/local/local.go),
[Go message model](../../bindings/go/api/contracts.go),
[Go UDP](../../bindings/go/udp/udp.go).

## Three workloads need distinct paths

1. **Use the whole object.** Build the final language object once. A direct codec
   can remove intermediate JSON and Rust trees, but cannot remove the caller's
   dictionary, JavaScript object, or Go struct. Measure against each runtime's
   optimized JSON implementation before adopting a custom traversal.
2. **Read a few fields.** Keep one owned binary frame and obtain requested fields
   together. Validate readable input once, without JSON export, before making
   claims about skipped work. A binary validator still walks the data; field
   selection does not automatically make validation proportional to selected fields.
3. **Forward or record a frame.** Carry encoded bytes and numeric frame identity
   through the boundary without building an object or formatting a timestamp.
   Use an explicit small append receipt when a full returned message adds no value.

“Zero copy” needs a named boundary. The pool currently copies a selected frame
under a storage lock to give the caller a stable snapshot. A Python memoryview
or Node Buffer can avoid a subsequent copy of that snapshot; it does not remove
the pool copy. Converting a binary string to a Python or JavaScript string still
needs runtime work. Keeping a frame alive for one field can also retain a large
allocation, so peak memory matters alongside throughput.

## Runtime rules that shape the proposals

Python's buffer protocol can retain an exporter while a view uses its bytes.
A native extension must manage Python thread state, hold the global interpreter
lock (GIL) where the build requires it for object access, and detach around
blocking native work. The current ctypes library already releases the GIL for
CDLL calls; replacing ctypes does not create that benefit. The package supports
Python 3.10+, while buffer APIs entered the Stable ABI in 3.11, so an extension
must account for packaging and version support. See the official
[buffer contract](https://docs.python.org/3/c-api/buffer.html),
[thread-state rules](https://docs.python.org/3/c-api/threads.html), and
[ctypes calling behavior](https://docs.python.org/3/library/ctypes.html#ctypes.CDLL).

Node-API can expose external buffer memory with a finalizer. That finalizer must
call Plasmite's deallocator exactly once, including creation failure and teardown;
it cannot use libc `free` on a Rust allocation. Some hosts disallow external
buffers, so preserve a copy fallback. JavaScript values belong to their Node
environment; background native work must return owned results to the environment
thread before creating objects. See [Node-API](https://nodejs.org/api/n-api.html#napi_create_external_buffer)
and [async work](https://nodejs.org/api/n-api.html#asynchronous-operations).

Go's cgo rules already permit borrowing pointer-free byte storage during a call.
Keeping Go memory after return introduces pinning requirements; returning a slice
over C memory introduces explicit lifetime requirements. Neither makes an
ordinary Go slice automatically own C memory. Prefer safe Go-owned output until
measurements justify a separate scoped ownership API. See the official
[cgo pointer rules](https://pkg.go.dev/cmd/cgo#hdr-Passing_pointers).

Rust can express a borrow from an owned snapshot through a lifetime. Serde can
target a binary representation and deserialize borrowed values without requiring
`Value` as an intermediate. The format and types must permit those borrows;
alignment, strings, and numeric range still matter. See
[Serde serializers](https://serde.rs/impl-serializer.html) and
[deserializer lifetimes](https://serde.rs/lifetimes.html).

## Shared work and priorities

Start with a compact cross-language measurement harness and cheap changes:
the remaining Python byte copies, Go's duplicate owned JSON copy and tag calls,
Node's ABI-output copy, and optional raw UDP consumption. Establish complete
producer-to-consumer timings and count conversions, allocations, and runtime
crossings. A faster raw API alone does not establish a faster ordinary API.

The shim already has `verify_json_tree`, a checked binary traversal used before
JSON export. Reuse it and fill the readability gaps that yyjson writing and
Rust scanning currently catch, including non-finite floats and invalid UTF-8.
C/binding raw reads already skip this export; binary append and Rust/CLI raw
selection pay it. Avoid allocating tags for unfiltered selection while retaining
metadata validation.

The next shared targets remove more work: bypass Rust value trees in C JSON
operations, parse JSON once across encoder growth, add a direct Serde codec,
validate binary readability without exporting JSON, and return small committed
receipts. JSON append normalization and JSON read output need separate
compatibility corpora. Splicing yyjson output into an envelope can change serde
key ordering, numeric formatting or escapes. A small receipt does not resolve
append input normalization. Reuse the current storage engine and owned frames. Prototype native
Python and Node object bridges with checked Lite3 writer/reader accessors.
Rust Serde serves Rust callers; it need not sit between Python/JavaScript
objects and Lite3. Reuse the storage and validation engine across these paths.

Coarse field access and explicit bounded batches can amortize runtime crossings.
A batch needs stated commit and partial-failure behavior, limits on bytes and
records, and per-message committed identities. Keep single-message calls immediate.
Introduce no implicit buffering delay to improve a throughput number.

The remaining latency target sits below every binding: Unix notification waits
currently retry `sem_trywait` with a 5 ms sleep. Prototype native waits for each
supported operating system. Measure writer semaphore-handle reuse separately.
Node UDP also has a distinct JavaScript polling loop with sleeps up to 10 ms;
it needs its own asynchronous-wait experiment. The prior 1 ms polling experiment reduced latency
but raised idle CPU; it does not justify a global polling change.

Remote work must follow actual SDK support. Rust already offers HTTP Lite3
append/get/tail; Node's HTTP client currently offers JSON. Python and Go expose
local and UDP operations here. Reuse existing binary HTTP endpoints where a
Node application forwards or projects frames; move Rust's already parsed payload tree into tail messages instead of
rebuilding its nested containers. Do not expand Python/Go HTTP support as a performance task.
Across User Datagram Protocol (UDP) bindings, make full-message decoding optional
without changing arrival-order delivery, duplicates, late packets, explicit gap
events, or the lack of per-send commit acknowledgement.

## Evidence and acceptance

The existing [pipe comparison](../performance/pipe-comparison.md) measured
46,208 messages/s for ordinary Python calls and 296,838 for a buffered pipe in
the follow-up workload. The pipe batches bytes and retains no history; Plasmite
commits independent frames, returns receipts, and retains bounded history. The
gap cannot serve as a predicted gain from conversion removal. Paced measurements
and idle CPU belong beside saturated throughput.

Each proposal needs its own before/after evidence with source and library hashes,
the same caller-visible work, full output and receipt checks, and ordinary API
controls. Report whole-object, selected-field, and raw forwarding separately.
Use small sensor, wide/nested, and large-string cases where relevant; add only
the concurrency or runtime stress that tests the proposed change. Separate
diagnostic allocation counts from uninstrumented timing. Do not claim a win from
overlapping noisy samples or transfer work into GC, an event loop, or finalizers
without counting it.

Preserve JSON and numeric behavior, UTF-8 and embedded-NUL handling, malformed
input errors before commit, exact committed identity, raw bytes where the API
promises them, bounded memory, retention gaps, filter limits, cancellation, and
file identity/locking rules. Add APIs compatibly and document any deliberate
semantic choice before implementation. The prior
[message-model study](implementation-distillation/message-path.md) rejected a
universal lazy message abstraction; this proposal addresses concrete conversions.

All tickets begin as drafts. An experiment can conclude that the current path
wins; record that result rather than shipping complexity without a measured gain.

## Existing decisions to preserve

Completed spike `3PQTKC` found shape-dependent encoder retries and recommended
keeping geometric growth while fixing the final cap attempt (`EYO5BU`, complete).
The new parse-once proposal needs new complete-path and memory evidence to
justify native document lifetime machinery. Completed `X7RXIS` retained Lite3
and its checked native boundary. Neither decision forbids a measured improvement.

Payload numbers and frame identities have different representations. The current
yyjson decoder stores unsigned payload integers above `i64::MAX` as floating
point; envelope sequence and timestamp fields remain unsigned 64-bit identities.
Ordinary JSON append returns a clone of the original Value as receipt data.
Rebuilding that receipt from stored Lite3 could change payload numbers.

## Suggested first work

Run the compact measurement baseline alongside binary validator reuse and minimal
receipts. Move the existing payload Value in Rust HTTP tails; the current
from_value conversion rebuilds every nested map/array even though no JSON text
reserialization occurs. Next test Node's cheap native-envelope parser and output ownership,
Go's duplicate copies/tag calls, and Python's remaining byte overhead. Then
measure the C JSON read bridge and writer semaphore-handle reuse. Keep native
object codecs, broader field APIs, scheduling bridges and batches as measured
prototypes; early results should set their budget and design.

Use one writer and three readers as a small fan-out control. One semaphore post
releases one waiter; a single-reader latency result does not describe broadcast
latency. State removable work and a measured budget or bounded hypothesis for
each optimization. Diagnostic medians cannot supply an additive speedup forecast.

## Ergo work tickets

Epic **NEYB7J — Make every language boundary fast end to end** contains these
23 draft proposals. Run `ergo show ID` for full acceptance criteria.

| Ticket | Proposed improvement |
| --- | --- |
| QXY2OM | Measure end-to-end costs across every language binding |
| A2YASO | Remove Rust value-tree round trips from C JSON reads |
| VXOTFT | Encode and decode Rust values directly through Lite3 |
| M2FNWG | Avoid reparsing JSON when Lite3 output buffers grow |
| Q2YAUP | Validate readable Lite3 data without exporting JSON |
| 6N22SX | Expose coarse field reads over owned Lite3 frames |
| UO7FVO | Add small committed receipts for JSON and binary appends |
| BTS4ZI | Remove remaining Python byte-path overhead |
| F5MEFX | Prototype a native Python object and buffer boundary |
| MJAQRU | Transfer native output buffers into Node without copying |
| GSYEYP | Prototype direct Node object conversion with exact message identity |
| WBA6TN | Reduce Node stream scheduling cost and shared-worker contention |
| NTY2GK | Remove duplicate Go JSON copies and per-tag cgo allocations |
| ECNBVJ | Add typed and selected-field Go reads from Lite3 |
| UJHEZU | Prototype explicit bounded append and read batches across bindings |
| 7CA5OC | Replace polling notification waits with measured native wakeups |
| 27CHGI | Make full-message decoding optional for raw UDP consumers |
| WVJROC | Expose existing binary HTTP operations in the Node SDK |
| 2UB4UH | Parse JSON append input once while preserving normalization |
| 7ECLTU | Avoid whole-tree revivers for canonical native Node messages |
| KAUJM5 | Reuse writer semaphore handles when measured gains justify it |
| IXNZWI | Reduce the Node UDP follower polling delay |
| 32VKBS | Move the existing data tree into Rust HTTP tail messages |

Only the C field interface (`6N22SX`) is a hard prerequisite for its Go consumer
(`ECNBVJ`). Binary validation and accessor/codec results inform other experiments
without blocking them. Python and Node prototypes do not require Rust Serde.
A finished experiment can recommend keeping current code, so downstream work
must inspect the result before assuming an API exists.
