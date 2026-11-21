# Plasmite for Python

Plasmite stores JSON messages in pools: bounded ring buffers on disk that many processes can write and read at once. `feed` writes a message, `follow` waits for new ones, and `fetch` reads what the pool holds and stops. Each message carries a sequence number (`seq`), a time, tags and a JSON object as `data`. When a pool fills, the oldest messages make room for new ones. This package opens local pools, pools on a shared HTTPS server and UDP endpoints. The [project README](https://github.com/sandover/plasmite#readme) explains the model and the CLI.

## Install

```bash
uv add plasmite            # the library, in a uv project
uv tool install plasmite   # the plasmite CLI on its own
```

The package needs Python 3.10 or newer. Wheels for macOS (x86_64, arm64), Linux x86_64 (glibc 2.35 or newer) and Windows x86_64 bundle the native library and the `plasmite` CLI. See [distribution](https://github.com/sandover/plasmite/blob/main/docs/record/distribution.md) for other platforms and the C SDK.

## Quick start

Record a temperature reading, watch for it, then read recent history.

```python
from plasmite import Client

with Client("./data") as client, client.open_pool("sensors", create=True) as pool:
    with pool.follow(tags=["kitchen"]) as live:
        pool.feed({"temp_c": 21.4}, tags=["kitchen"])
        print(next(live).data)                     # {'temp_c': 21.4}
    with pool.fetch(last=10) as recent:
        for message in recent:
            print(message.seq, message.tags, message.data)
```

`follow` fixes its position before it returns, so it sees the feed that comes after it. `fetch` stops at the last message present when it opened. `message.time` holds Unix nanoseconds as an integer, and `pool.info()["bounds"]` reports the oldest and newest retained sequence. The module functions `open_pool`, `create_pool`, `list_pools` and `delete_pool` use the default directory, `~/.plasmite/pools`. Use `create_pool(ref, size_bytes)` to choose a size; `create=True` makes a 1 MiB pool.

To describe what a pool carries, pass `about=` and `example=` to `create_pool`.

```python
with client.create_pool("sensors", about="Kitchen readings", example={"temp_c": 21.4}) as pool:
    print(pool.info()["about"])
    pool.set_description(about="Kitchen and pantry readings")
```

`info()` and `list_pools()` include any description fields. `set_description()` replaces both fields and clears any you omit. The example helps readers understand the data they can expect.

## Reading

Every read starts at a position. `follow` starts at the next new message by default; `fetch` starts at the oldest retained one.

```python
from datetime import timedelta
from plasmite import at

pool.fetch(since="oldest")                    # everything the pool still holds
pool.fetch(since=42)                          # sequence 42 onward, inclusive
pool.fetch(since="5m")                        # or since=timedelta(minutes=5)
pool.fetch(since="2026-11-19T09:00:00Z")      # an RFC 3339 time
pool.fetch(since=at(1_795_078_800_000_000_000))  # the same time as Unix nanoseconds
pool.fetch(last=20)                           # the 20 most recent messages
pool.follow(tags=["error"])                   # only messages carrying every listed tag
```

`follow` takes the same arguments. Pass `since` or `last`, not both.

Resume after a restart by saving the next sequence to read. If the pool overwrote messages before you read them, opening or advancing the reader can raise `RetentionGapError`.

```python
from pathlib import Path
from plasmite import RetentionGapError

checkpoint = Path("sensors.seq")
start = int(checkpoint.read_text()) if checkpoint.exists() else "oldest"
while True:
    try:
        with pool.follow(since=start) as reader:
            for message in reader:
                print(message.data)
                checkpoint.write_text(str(message.seq + 1))
    except RetentionGapError as gap:
        print(f"lost {gap.missing}..{gap.next_seq - 1}; resuming at {gap.next_seq}")
        start = gap.next_seq
        checkpoint.write_text(str(start))
```

`gap.missing` is the first sequence you missed and `gap.next_seq` the oldest one still retained. Saving `next_seq` accepts the loss and moves on.

To wait for a bounded time, call `reader.next(wait)` with seconds or a `timedelta`. It returns `None` when the wait expires and leaves the reader open; `reader.next(0)` polls. A finished reader raises `StopIteration`.

## HTTPS pools and UDP

To use a pool on a shared server, connect once with `plasmite access connect https://pools.example.net:9743`, then open the pool by URL. `list_pools()` still lists the local directory.

```python
import plasmite

with plasmite.open_pool("https://pools.example.net:9743/sensors", create=True) as pool:
    pool.feed({"temp_c": 21.4}, tags=["kitchen"])
```

UDP trades delivery guarantees for low overhead. A send returns `None` once the operating system accepts the datagram. Reads deliver messages in arrival order, including duplicates and late arrivals. Before running this example, create `sensors` with `plasmite pool create sensors` and run `plasmite serve --udp-follow` in another terminal.

```python
from plasmite import UdpClient

with UdpClient("udp://127.0.0.1:9701") as client, client.open_pool("sensors") as pool:
    pool.feed({"temp_c": 21.4}, tags=["kitchen"])
    with pool.follow(since="oldest") as reader:
        for message in reader:
            print(message.seq, message.data)
```

Read with `reader.events()` or `reader.next_event(wait=...)` to include `UdpSkip` for ranges the server skipped and `UdpProgress` for subscription progress. The [UDP spec](https://github.com/sandover/plasmite/blob/main/spec/udp/v1/SPEC.md) defines delivery and limits.

## Errors and threads

Native failures raise a subclass of `PlasmiteError` for each of the nine error kinds: `UsageError`, `NotFoundError`, `AlreadyExistsError`, `BusyError`, `PermissionDeniedError`, `CorruptError`, `IoError`, `RetentionGapError` and `InternalError`. Each carries `kind` and, when known, `path`, `seq` and `offset`. Bad read positions and tags on a forwarded message raise `UsageError`; checks done in Python, such as an empty pool name or a negative number, raise `ValueError` or `TypeError`.

Each `Client`, pool and reader serializes its own native calls. Separate handles can run concurrently. To stop a reader from another thread, call its `close()` or pass a `threading.Event` as `cancel` when you open it and set the event when you want to stop. Messages are independent Python values and outlive the pool.

## Message data and forwarding

`feed`, `fetch` and `follow` return the same `Message` type. The message owns its stored bytes. Read `seq`, `time`, `tags` and `sender` without decoding `data`. The first access to `data` decodes and caches a Python dict. The server records `sender` for writes made with an access key. Other writes return `None` for `sender`.

Use `message.field("temp_c")` to decode one top-level field. `message.field("unit", "celsius")` returns the supplied default when the field is absent. Once you have read `data`, `field` reads from that dict.

```python
with pool.fetch(last=1) as reader:
    message = next(reader)
print(message.seq, message.field("temp_c"))
archive.feed(message)                       # forward data and tags
archive.feed(message.data, tags=["review"]) # write the object with new tags
archive.feed(b'{"temp_c":21.4}', format="json")
archive.feed(message.raw, format="lite3")
```

`raw` is a read-only `memoryview` of the stored Lite3 payload. Pass `format="json"` or `format="lite3"` when feeding bytes. Forwarding a `Message` or Lite3 gives it a new sequence and time and uses the tags it already carries. Changes to the cached dict leave the stored bytes intact. Use `feed(message.data, tags=...)` to write the current contents of the dict.

## Async Python

Use `plasmite.aio` when your application already runs on asyncio. Local paths, HTTPS URLs, and pools opened through `aio.UdpClient` use the same `Message` and read options as the synchronous API.

```python
import asyncio
from plasmite import aio


async def main():
    async with aio.open_pool("handoffs", create=True) as pool:
        async with pool.follow(tags=["handoff"]) as reader:
            message = await reader.next(wait=5)
            if message is not None:
                print(message.data)


asyncio.run(main())
```

Use `async for message in pool.follow(...)` to wait for new messages, or use `pool.fetch(...)` to read through the sequence available when the reader opens. The starting position takes effect when you enter the reader's context, iterate it for the first time, or call its first `next()`. `await reader.next(wait=...)` returns `None` if its wait expires, including while the reader opens. A zero wait returns only a result that is already available. Cancelling a read, including through `asyncio.timeout()`, leaves the reader open. If a read completes as cancellation arrives, the next call can still receive the result. A second concurrent read on that reader raises `UsageError`. The reader closes at context exit or when you call `await reader.aclose()`. When a plain loop drops the last reference to a reader, its finalizer starts native cleanup.

Use `await pool.feed(data, tags=["handoff"])` to write. A local feed runs inline unless you request `durability="flush"`, which runs on a worker. HTTPS feeds run through the async HTTP transport. UDP feeds return `None` after the operating system accepts the datagram. A cancelled feed may already have written the message.

See the [asyncio benchmarks](benchmarks/asyncio.md) for measurements and reproduction commands.

## Building from source

From the repo root, build and stage the native library and run the binding tests.

```bash
just bindings-python-test
```

Set `PLASMITE_LIB_DIR` to require a specific library directory. Otherwise the package checks for the bundled library first, then searches the system library path. From `bindings/python`, run `PLASMITE_SDK_DIR=/path/to/sdk uv build --wheel` to build a wheel that bundles that SDK's library and CLI. Set `PLASMITE_PYTHON_VIEWS=1` to require the native view extension or `0` to build without it.
