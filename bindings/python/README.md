# Plasmite Python Bindings (v0)

These bindings wrap the `libplasmite` C ABI via `ctypes`.

## Installation

Published wheels bundle native assets under `plasmite/_native/` on the platforms where wheels are currently shipped:

- `libplasmite.(dylib|so)` for ctypes loading
- `plasmite` CLI binary (invoked via Python console script entrypoint)

Current packaged-wheel targets:
- macOS: `x86_64`, `arm64`
- Windows: `x86_64`
- Linux: `x86_64`, glibc 2.35 or newer (unreleased)

The Linux wheel bundles the CLI and library; no separate SDK or Rust build is
needed. Both `plasmite` and `pls` commands use the same bundled binary.
These Linux packaging changes are unreleased. Version 1.2.1 on Linux
still uses the source package and needs a system SDK (Homebrew or a GitHub SDK
archive). Wheels built by this checkout use the stable C ABI through ctypes and
support Python 3.10+ without depending on one CPython minor version.

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

Conformance runner (manifest parity with Rust/Go/Node):

```bash
cargo build -p plasmite
cd bindings/python
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" \
PLASMITE_BIN="$(pwd)/../../target/debug/plasmite" \
python3 cmd/plasmite_conformance.py ../../conformance/sample-v0.json
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

`Pool.append(...)`, `Pool.get(...)`, `Pool.tail(...)`, and `Pool.replay(...)` return typed `Message` values by default.

Use `append_json(...)` / `get_json(...)` only when you explicitly need raw wire bytes.

`Pool.tail(..., tags=[...])` uses exact tag matching and composes with other filters using AND semantics.

## UDP sender and follower

The UDP client names its server endpoint explicitly. `send_json` and
`send_lite3` return no storage receipt: success means the operating system
accepted the datagram. A follower yields original Lite3 frames, sequence gaps,
and progress records. The server must opt in with `serve --udp-follow`.

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

Each opened stream owns an independent handle. `tail()` and `replay()` release
handle locks between reads, including while yielding messages or sleeping for
replay timing. Consume each generator from one thread at a time.

`close()` waits for an active native call to finish; it does not interrupt a
blocking stream read. Pass a finite `timeout_ms` when opening a stream if another
thread needs to close it within a bounded wait. A stream without a timeout can
keep `close()` waiting indefinitely.
