# Plasmite Python Bindings (v0)

These bindings wrap the `libplasmite` C ABI via `ctypes`.

## Installation

Wheels are designed to bundle native assets under `plasmite/_native/`:

- `libplasmite.(dylib|so)` for ctypes loading
- `plasmite` CLI binary (invoked via Python console script entrypoint)

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

## Error behavior

- Invalid local arguments raise `ValueError` / `TypeError`.
- ABI/runtime failures raise typed subclasses of `PlasmiteError` (`NotFoundError`, `AlreadyExistsError`, `BusyError`, `PermissionDeniedError`, `CorruptError`, `IoError`, `UsageError`, `InternalError`).
- All `PlasmiteError` values expose `kind`, `path`, `seq`, and `offset` when present.

## Troubleshooting

- **Missing pool directory**: pool creation creates parent directories automatically. If you call `open_pool(...)` on a missing pool, catch `NotFoundError` or use `client.pool(...)` to create-or-open.
- **Permission denied**: choose a writable pool directory (`Client("/path/to/pools")`) and verify directory permissions/ownership. Errors include `err.path` when available.
