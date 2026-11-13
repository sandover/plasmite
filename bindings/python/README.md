# Plasmite Python Bindings (v0)

These bindings use the `libplasmite` C ABI. Native CPython container views
provide lazy access to immutable Lite3 snapshots; the existing `Message` API
keeps its editable native dictionaries.

## Installation

Published wheels bundle native assets under `plasmite/_native/` on the platforms where wheels are currently shipped:

- `libplasmite.(dylib|so)` for ctypes loading
- `plasmite` CLI binary (invoked via Python console script entrypoint)

Current packaged-wheel targets:
- macOS: `x86_64`, `arm64`
- Windows: `x86_64`
- Linux: `x86_64`, glibc 2.35 or newer (1.3.0 and later)

The Linux wheel bundles the CLI and library. Both `plasmite` and `pls` use the
bundled binary. Version 1.2.1 on Linux uses the source package and needs a
system SDK (Homebrew or a GitHub SDK archive). Wheels since 1.3.0 use the
stable C ABI and support Python 3.10 and newer.

### Using uv (recommended)

```bash
uv tool install plasmite
```

For project dependencies in a `uv`-managed workspace:

```bash
uv add plasmite
```

Runtime load order is:
1. `PLASMITE_LIB_DIR` override (development escape hatch)
2. Bundled `plasmite/_native/libplasmite.(dylib|so)` absolute path
3. System linker fallback (`plasmite` / `libplasmite`)

For development/testing from this repo, see Install & Test below.

## Build Requirements

- Python 3.10+
- `libplasmite` built from this repo (`cargo build -p plasmite`) for local development

## Install & Test

From the repo root:

```bash
cargo build -p plasmite
```

Canonical repo-root command:

```bash
just bindings-python-test
```

Equivalent manual commands (from `bindings/python`):

```bash
uv venv
source .venv/bin/activate
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" uv pip install -e .
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" PLASMITE_BIN="$(pwd)/../../target/debug/plasmite" python3 -m unittest discover -s tests
```

Build a wheel with bundled native assets from a specific SDK directory:

```bash
PLASMITE_SDK_DIR=/path/to/sdk uv build
```

## Usage

```python
from plasmite import Client, Durability, NotFoundError

with Client("./data") as client:
    with client.pool("docs", 64 * 1024 * 1024) as pool:
        msg = pool.append({"kind": "note", "text": "hi"}, ["note"], Durability.FAST)
        print(msg.seq, msg.tags, msg.data["text"])

        fetched = pool.get(msg.seq)
        print(fetched.data["text"])

        for item in pool.tail(tags=["note"], max_messages=1, timeout_ms=100):
            print(item.seq, item.tags, item.data)

    try:
        client.open_pool("missing")
    except NotFoundError:
        print("pool not found")
```

`Pool.append(...)`, `Pool.get(...)`, and `Pool.tail(...)` return typed `Message` values by default.

Use `append_json(...)` / `get_json(...)` only when you explicitly need raw wire bytes.

`Pool.tail(..., tags=[...])` uses exact tag matching and composes with other filters using AND semantics.

## Immutable message views

Use `get_view` when you need selected fields or want to retain encoded data:

```python
from plasmite import to_json, to_native

view = pool.get_view(seq)
power = view.data["metrics"]["power"]
for sample in view.data["samples"]:
    print(sample)
last_two = view.data["samples"][-2:]  # lazy readonly slice
editable = to_native(view.data)       # independent dict/list tree
wire_json = to_json(view)             # direct formatting; no native data tree
encoded = memoryview(view)            # readonly Lite3 payload, without identity
```

`ObjectView` implements `collections.abc.Mapping`; `ArrayView` implements
`Sequence`, including negative indices and slices. Missing keys raise `KeyError`;
JSON null becomes `None`. Field names cannot collide with conversion helpers,
because `to_native` and `to_json` are module functions. Views reject assignment.
Strings and numbers become Python scalars when you request them. Byte values
expose readonly memoryviews; `to_native` makes owned `bytes`.

A view owns one independent snapshot. Children, slices, iterators and buffers
keep that snapshot alive after you close the pool or overwrite its ring slots.
A small retained child retains the entire message. `memoryview(view)` includes
stored meta/data bytes; `seq` and `timestamp_ns` remain separate identity fields.
Use `view_from_lite3(payload, seq=0, timestamp_ns=0)` to validate and snapshot
external encoded bytes. Mutable inputs trigger a defensive copy.

`view.time_rfc3339` preserves nanoseconds. `view.time` creates a UTC `datetime`
with Python's microsecond precision. `to_native(view)` and `to_json(view)` export
the canonical `seq`, `time`, `meta.tags`, `data` envelope. Whole-message
exports omit unknown stored metadata fields; `view.meta` and subtree conversion
preserve them. `json.dumps` can consume a
subtree with `default=to_native`; that route explicitly builds native containers.

### Direct native input

`pool.append_view(data, tags=None, durability=Durability.FAST)` walks exact
builtin dictionaries, lists, tuples and scalars into Lite3 and retains that
encoding as its result. It copies byte values from `bytes`, `bytearray` and
memoryviews directly into the encoding. Strided buffers copy their logical byte
selection into a temporary contiguous buffer first. Later input mutations
cannot change the result. The data root accepts an exact dictionary, `ObjectView`, or `MessageView`; keys
and tags must be exact strings. Integers must fit signed 64 bits, floats must be
finite, and keys cannot contain NUL. String values and tags can contain NUL.

The method rejects custom values and builtin subclasses without invoking their
hooks. Keep `append` for its existing JSON/dataclass behavior, or convert custom
input explicitly. The tag iterable runs once. Native encoding holds Python's
interpreter state while reading input, then releases it during storage work.
Do not mutate input concurrently with encoding.

The fallback snapshots builtin input before its ctypes calls. It uses compact
bulk JSON for ordinary JSON-compatible data; byte values and NUL tags use the
checked builder through ctypes. These routes preserve view semantics and pay
extra copying and runtime-call costs.

### Streaming and forwarding

```python
with pool.tail_views(tags=["sensor"], since_seq=1) as stream:
    message = next(stream)                    # acquires exactly one snapshot
    batch = stream.next_batch(16, 256 * 1024)  # explicit count and encoded-byte bounds
    for message in batch:
        print(message.seq, message.data["power"])
        destination.append_view(message)     # reuses the immutable encoding
```

`next_batch` waits for the first accepted message, then takes available records
without waiting to fill the batch. `timeout_ms=0` polls and returns `[]` when no
record is ready. A finite timeout also returns `[]` when its wait expires;
completion or cancellation raises `StopIteration`. Stream-wide timeouts, gaps,
sequence positions and accepted-message limits follow the existing tail rules.
A first record larger than the byte bound raises an error without consuming it;
retry with a larger bound. Returned children keep their individual owners alive
when the stream, pool, batch list or other messages go away.

Forwarding a `MessageView` with omitted tags preserves its encoding and tags.
Explicit tags rebuild the message around its encoded data. Passing an
`ObjectView` as the root, or nesting object/array views in builtin input, copies
the checked subtrees directly into the destination encoding. Array and byte
slices contribute their selected elements. These copies avoid native container
construction and JSON conversion.

Batching helps a busy reader amortize lock handoffs. It adds overhead to a
reader that usually receives one record at a time; normal iteration never
prefetches or batches behind the caller's back.

### Runtime and wheel support

Standard CPython 3.11+ uses one `cp311-abi3` extension wheel per platform.
CPython 3.10 uses a `cp310-cp310` wheel because its stable ABI does not expose
the buffer protocol. Source archives carry the C source and public SDK header;
native source builds require a C compiler. Build with
`PLASMITE_PYTHON_VIEWS=1` to require native views or `=0` to choose the functional
ctypes fallback. Keep the SDK library and header from the same build.

PyPy and free-threaded CPython use the fallback. Free-threaded Python never
imports this extension and keeps the GIL disabled. The fallback preserves lazy
immutable containers and readonly buffers, while paying for extra encoded
copies and per-field ctypes calls. Native view objects support one interpreter
per process; additional interpreters use fallback where the Python runtime
permits the underlying ctypes SDK. No cross-interpreter object sharing exists.
The explicit fallback wheel carries the Python-independent `py3-none` tag and
the same platform SDK binaries.

The [measured native/fallback costs](../../docs/performance/language-boundary-results/owned-views/python/README.md)
include regressions. Native views help selected cold reads; retained Python
lists and dictionaries win repeated scans and lookup. Full wide-object native
conversion can cost more than the existing JSON API. Choose the result your
consumer needs and include conversion cost in comparisons.

## UDP sender and follower

The UDP client names its server endpoint explicitly. `send_json` and
`send_lite3` return no storage receipt: success means the operating system
accepted the datagram. A follower yields original Lite3 frames, explicit server
skip notices, and progress records. The server must opt in with `serve --udp-follow`.
The [receive policy](../../spec/udp/v1/SPEC.md#receive-ordering-and-notices)
requires arrival-order delivery, including late messages and duplicates;
sequence/progress jumps never establish loss or suppress frames. This source
change awaits a major release. Finite follows end on terminal progress and may
leave DATA in flight.

```python
from plasmite import UdpClient, UdpFollowOptions, UdpFrame, UdpStart

with UdpClient("udp://127.0.0.1:9701") as client:
    with client.open_pool("sensors") as pool:
        with pool.follow(UdpFollowOptions(start=UdpStart.new(), live=True)) as follow:
            pool.send_json({"temperature": 23}, tags=["sensor"])
            for event in follow:
                if isinstance(event, UdpFrame):
                    message = event.message()
                    print(message.seq, message.data)
```

Use `UdpStart.last(count)`, `UdpStart.since_ns(timestamp_ns)`, or
`UdpStart.sequence(seq)` to select retained history. Pass a finite
`timeout_ms` to `next_event` when a caller needs a bounded wait; `close()` and
`cancel()` interrupt an active wait within 50 milliseconds.

Pools have bounded retention. A tail normally resumes at the next retained
message if writes overtake it. Set `error_on_gap=True` when the consumer must
distinguish that loss from an ordinary lack of matching messages:

```python
from plasmite import RetentionGapError

try:
    for item in pool.tail(since_seq=last_seq + 1, error_on_gap=True):
        process(item)
except RetentionGapError as err:
    print(f"rebuild required; first missing sequence: {err.seq}")
```

## Error behavior

- Invalid local arguments raise `ValueError` / `TypeError`.
- ABI/runtime failures raise typed subclasses of `PlasmiteError` (`NotFoundError`, `AlreadyExistsError`, `BusyError`, `PermissionDeniedError`, `CorruptError`, `IoError`, `UsageError`, `InternalError`, `RetentionGapError`).
- All `PlasmiteError` values expose `kind`, `path`, `seq`, and `offset` when present.

## Troubleshooting

- **Missing pool directory**: pool creation creates parent directories automatically. If you call `open_pool(...)` on a missing pool, catch `NotFoundError` or use `client.pool(...)` to create-or-open.
- **Permission denied**: choose a writable pool directory (`Client("/path/to/pools")`) and verify directory permissions/ownership. Errors include `err.path` when available.

### Threads and handle ownership

The Python wrapper serializes native operations and `close()` on each `Client`,
`Pool`, `Stream`, and `Lite3Stream` instance. You can share an instance between
threads without racing its native handle or freeing it during a call. Independent
handles can operate concurrently. Message values are owned snapshots and can
outlive the pool.

Each opened stream owns an independent handle. `tail()` releases handle locks
between reads, including while yielding messages. Consume each generator from
one thread at a time.

A stream's `cancel()` interrupts a read while it waits for messages; `close()`
cancels first, then waits for the active call and its result copy before freeing
the handle. `next_wait(timeout_ms)` returns `(status, value)`: 0 means wait expired,
1 carries a message/frame, 2 means finished, and 3 means cancelled. A zero budget
polls once; wait expiry retains position. The original open-time `timeout_ms`
still bounds the stream's lifetime. High-level `tail(cancel=event)` accepts a
`threading.Event` and checks it during bounded waits. Cancellation cannot interrupt
an operating-system file lock already in progress.

For playback timing, use ordinary streams and an application scheduler; see the
[next major upgrade guide](../../docs/record/upgrading-next-major.md#sdk-applications-control-playback-timing).

## Owned UDP receiving

```python
with udp_pool.follow_views(options) as events:
    for event in events:
        if isinstance(event, plasmite.MessageView):
            print(event.seq, event.data["temperature"])
```

DATA arrives as an immutable message view; gaps and progress use the existing
event classes. A retained message stays readable after the follow closes.
Field access reads Lite3 directly; `plasmite.to_native(event.data)` creates an
independent native value. UDP keeps arrival order, including duplicate and late
packets. The new methods require a matching native library.

## Numeric append receipts

```python
receipt = pool.append_identity({"temperature": 23.5}, tags=["sensor"])
print(receipt.seq, receipt.timestamp_ns)  # Unix nanoseconds, exact integers
```

`append_json_identity` accepts JSON bytes and `append_lite3_identity` accepts
encoded Lite3 bytes. They return `AppendReceipt` without echoing or decoding the
data. Existing `append` returns a full Message. New identity methods require a
matching native library; older libraries raise `NotImplementedError` before
commit. Input validation and durability follow the existing append methods.
