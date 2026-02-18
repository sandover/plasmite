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

## Usage

```python
from plasmite import Client, Durability, parse_message

with Client("./data") as client:
    with client.create_pool("docs", 64 * 1024 * 1024) as pool:
        message = pool.append({"kind": "note", "text": "hi"}, ["note"], Durability.FAST)
        parsed = parse_message(message)
        print(parsed)

        fetched = parse_message(pool.get(parsed["seq"]))
        print(fetched["data"]["text"])

        stream = pool.open_stream(since_seq=parsed["seq"], max_messages=1, timeout_ms=100)
        for item in stream:
            print(parse_message(item))
        stream.close()

        for item in pool.tail(tags=["note"], max_messages=1, timeout_ms=100):
            print(parse_message(item))
```

`Pool.get(seq)` is an alias for `Pool.get_json(seq)`.

`Stream` and `Lite3Stream` implement Python iterator protocol, so `for item in stream:` works directly.

`Pool.tail(..., tags=[...])` uses exact tag matching and composes with other filters using AND semantics.

## Error behavior

- Invalid local arguments raise `ValueError` / `TypeError`.
- ABI/runtime failures raise `PlasmiteError` with `kind`, `path`, `seq`, and `offset` when present.
