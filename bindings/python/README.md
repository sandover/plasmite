# Plasmite Python Bindings (v0)

These bindings wrap the `libplasmite` C ABI via `ctypes`.

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
stable C ABI through ctypes and support Python 3.10 and newer.

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
PLASMITE_SDK_DIR=/path/to/sdk python -m build
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
