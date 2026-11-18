# Plasmite Python Bindings (v0)

These bindings use the `libplasmite` C ABI. Native CPython container views
provide lazy access to immutable Lite3 snapshots; the existing `Message` API
keeps its editable native dictionaries.

## Installation

Published wheels bundle native assets under `plasmite/_native/` on the platforms where wheels are currently shipped:

- `libplasmite.2.dylib / libplasmite.so.2 / plasmite2.dll` for ctypes loading
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
2. Bundled `plasmite/_native/libplasmite.2.dylib / libplasmite.so.2 / plasmite2.dll` absolute path
3. System search for the versioned 2.0 filename

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
from plasmite import Client

with Client("./data") as client, client.open_pool("events", create=True) as pool:
    with pool.follow(tags=["sensor"]) as live:
        stored = pool.feed({"temperature": 23}, tags=["sensor"])
        received = next(live)
        print(received.seq, received.time, received.data)
    with pool.fetch(since=stored.seq) as history:
        print(next(history).data)
    print(pool.info()["bounds"])
```

`follow()` starts with new messages. `fetch()` reads retained history and
stops at the last sequence present when the reader opens. Pass `since` or
`last` to choose a starting point. `time` gives Unix nanoseconds as an integer.

Use `create_pool` for a custom size. The module functions `open_pool`,
`create_pool`, `list_pools`, and `delete_pool` use the default directory.

## HTTPS pools

After saving a connection with `plasmite access connect https://pools.example.net:9743`, open the pool by its URL.

```python
with plasmite.open_pool("https://pools.example.net:9743/events", create=True) as pool:
    pool.feed({"ready": True}, tags=["status"])
    with pool.fetch(last=1) as reader:
        print(next(reader).data)
```

`create_pool(ref, size_bytes)` also accepts an HTTPS reference.

## Immutable message views

Use `feed_view`, `fetch_view`, and `follow_view` to keep encoded data across
reads and forwarding. Convert views when you need editable containers. See the
[2.0 upgrade guide](../../docs/record/upgrading-2.0.md#keep-encoded-data-with-owned-views)
for cross-language ownership and numeric rules.

Open a `fetch_view` at a sequence when you need selected fields:

```python
from plasmite import to_json, to_native

with pool.fetch_view(since=seq) as reader:
    view = next(reader)
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
stored meta/data bytes; `seq` and `time` remain separate identity fields.
Use `view_from_lite3(payload, seq=0, time=0)` to validate and snapshot
external encoded bytes. Mutable inputs trigger a defensive copy.

`view.time` gives Unix nanoseconds. `to_native(view)` and `to_json(view)` export
the canonical `seq`, `time`, `meta.tags`, `data` envelope. Whole-message
exports omit unknown stored metadata fields; `view.meta` and subtree conversion
preserve them. `json.dumps` can consume a
subtree with `default=to_native`; that route explicitly builds native containers.

### Direct native input

`pool.feed_view(data, tags=None, durability=Durability.FAST)` walks exact
builtin dictionaries, lists, tuples and scalars into Lite3 and retains that
encoding as its result. It copies byte values from `bytes`, `bytearray` and
memoryviews directly into the encoding. Strided buffers copy their logical byte
selection into a temporary contiguous buffer first. Later input mutations
cannot change the result. The data root accepts an exact dictionary, `ObjectView`, or `MessageView`; keys
and tags must be exact strings. Integers must fit signed 64 bits, floats must be
finite, and keys cannot contain NUL. String values and tags can contain NUL.

The method rejects custom values and builtin subclasses without invoking their
hooks. Convert custom input explicitly before feeding it. The tag iterable runs
once. Native encoding holds Python's
interpreter state while reading input, then releases it during storage work.
Do not mutate input concurrently with encoding.

The fallback snapshots builtin input before its ctypes calls. It uses compact
bulk JSON for ordinary JSON-compatible data; byte values and NUL tags use the
checked builder through ctypes. These routes preserve view semantics and pay
extra copying and runtime-call costs.

### Streaming and forwarding

```python
with pool.follow_view(tags=["sensor"], since=1) as stream:
    message = next(stream)                    # acquires exactly one snapshot
    batch = stream.next_batch(16, 256 * 1024)  # explicit count and encoded-byte bounds
    for message in batch:
        print(message.seq, message.data["power"])
        destination.feed_view(message)       # reuses the immutable encoding
```

`next_batch` waits for the first accepted message, then takes available records
without waiting to fill the batch. `wait=0` polls and returns `[]` when no
record is ready. A finite timeout also returns `[]` when its wait expires;
completion or cancellation raises `StopIteration`. A retention gap raises
`RetentionGapError` with the missing and first retained sequences.
A first record larger than the byte bound raises an error without consuming it;
retry with a larger bound. Returned children keep their individual owners alive
when the stream, pool, batch list or other messages go away.

Forwarding a `MessageView` preserves its encoding and tags. Use its `data` as
the input if you need to supply new tags. Passing an
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

The UDP client names its server endpoint explicitly. `feed` and
`feed_lite3` return no storage receipt. Success means the operating system
accepted the datagram. A follower yields original Lite3 frames, explicit server
skip notices, and progress records. The server must opt in with `serve --udp-follow`.
The [receive policy](../../spec/udp/v1/SPEC.md#receive-ordering-and-notices)
requires arrival-order delivery, including late messages and duplicates;
sequence or progress jumps never establish loss or suppress frames. Fetch
ends on terminal progress and may
leave DATA in flight.

```python
from plasmite import UdpClient, UdpFrame

with UdpClient("udp://127.0.0.1:9701") as client:
    with client.open_pool("sensors") as pool:
        with pool.follow() as follow:
            pool.feed({"temperature": 23}, tags=["sensor"])
            for event in follow:
                if isinstance(event, UdpFrame):
                    message = event.message()
                    print(message.seq, message.data)
```

Use `last=count`, `since=at(timestamp_ns)`, or `since=seq` to select retained
history. Use `next(wait)` with a duration in seconds or a `timedelta` to bound a wait. `close()` and
`cancel()` interrupt an active wait within 50 milliseconds.

If a reader detects a retention gap, it raises `RetentionGapError` with the
first missing and first retained sequences.

```python
from plasmite import RetentionGapError

try:
    with pool.follow(since=last_seq + 1) as reader:
        for item in reader:
            process(item)
except RetentionGapError as err:
    print(f"Missing from {err.missing}. Retained history starts at {err.next_seq}.")
```

## Error behavior

- Invalid local arguments raise `ValueError` / `TypeError`.
- ABI/runtime failures raise typed subclasses of `PlasmiteError` (`NotFoundError`, `AlreadyExistsError`, `BusyError`, `PermissionDeniedError`, `CorruptError`, `IoError`, `UsageError`, `InternalError`, `RetentionGapError`).
- All `PlasmiteError` values expose `kind`, `path`, `seq`, and `offset` when present.

## Troubleshooting

- **Missing pool directory**. Pass `create=True` to `open_pool` to create a missing pool and its parent directories.
- **Permission denied**: choose a writable pool directory (`Client("/path/to/pools")`) and verify directory permissions/ownership. Errors include `err.path` when available.

### Threads and handle ownership

The Python wrapper serializes native operations and `close()` on each `Client`,
`Pool`, `Stream`, and `Lite3Stream` instance. You can share an instance between
threads without racing its native handle or freeing it during a call. Independent
handles can operate concurrently. Message values are owned snapshots and can
outlive the pool.

Each reader owns its handle and releases locks after each read. Use one thread per reader and a separate thread for feeding when needed. `next(wait)` accepts a duration in seconds or a `timedelta`. It returns `None` when the wait expires and raises `StopIteration` when the reader finishes or closes. Use `next(0)` to poll. `next_batch(max_count, max_bytes, wait)` uses the same wait and returns a list.

Pass `cancel=threading.Event()` to a reader on any transport, or call `close()` from another thread. Cancellation interrupts waiting for messages. An active file lock can finish first.

For playback timing, use ordinary streams and an application scheduler; see the
[2.0 upgrade guide](../../docs/record/upgrading-2.0.md#applications-control-playback-timing).

## Owned UDP receiving

```python
with udp_pool.follow_view(since="oldest") as events:
    for event in events:
        if isinstance(event, plasmite.MessageView):
            print(event.seq, event.data["temperature"])
```

DATA arrives as an immutable message view; skips and progress have separate event classes. A retained message stays readable after the follow closes.
Field access reads Lite3 directly; `plasmite.to_native(event.data)` creates an
independent native value. UDP keeps arrival order, including duplicate and late
packets. The new methods require a matching native library.

## Numeric feed receipts

```python
receipt = pool.feed_receipt({"temperature": 23.5}, tags=["sensor"])
print(receipt.seq, receipt.time)  # Unix nanoseconds, exact integers
```

`feed_json_receipt` accepts JSON bytes and `feed_lite3_receipt` accepts
encoded Lite3 bytes. Both return the committed sequence and time.

## Follow and fetch

Open with `pool.follow()` to start at the next new message. Use
`pool.fetch()` for finite history. Both take `since` or `last` and accept
`tags`. A read opens before it returns, so you can write a message after
calling `follow()` and before pulling the first item.

Pass a `threading.Event` as `cancel` or close the reader to stop. Use
`reader.next(timedelta(seconds=2))` to wait for one record without closing
the reader when the wait expires.
