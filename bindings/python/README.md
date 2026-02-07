<!--
Purpose: Document how to build and use the Plasmite Python bindings.
Exports: N/A (documentation).
Role: Quickstart for Python users of libplasmite.
Invariants: Uses the C ABI via ctypes and matches v0 semantics.
Notes: Requires libplasmite to be built and discoverable.
-->

# Plasmite Python Bindings (v0)

These bindings wrap the `libplasmite` C ABI via `ctypes`.

## Installation

**Note**: Pre-built wheels are not yet available. Installation from PyPI currently requires having the `plasmite` CLI already installed (which provides `libplasmite`).

### Using pip

```bash
# First install the CLI (provides libplasmite)
cargo install plasmite
# or: brew install sandover/tap/plasmite

# Then install the Python binding
pip install plasmite
```

### Using uv

```bash
# Install the CLI first
cargo install plasmite

# Then install with uv
uv pip install plasmite
```

The Python package uses ctypes to load the shared library built by the CLI. Ensure `libplasmite.dylib` (macOS) or `libplasmite.so` (Linux) is in a standard library search path or set `DYLD_LIBRARY_PATH`/`LD_LIBRARY_PATH`.

For development/testing from this repo, see Install & Test below.

## Build Requirements

- Python 3.10+
- `libplasmite` built from this repo (`cargo build -p plasmite`)

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
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" python -m pip install -e .
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" PLASMITE_BIN="$(pwd)/../../target/debug/plasmite" python3 -m unittest discover -s tests
```

On macOS, ensure `DYLD_LIBRARY_PATH` includes the same directory.
On Linux, set `LD_LIBRARY_PATH`.

## Usage

```python
from plasmite import Client, Durability

with Client("./data") as client:
    with client.create_pool("docs", 64 * 1024 * 1024) as pool:
        message = pool.append_json(
            b'{"kind":"note","text":"hi"}',
            ["note"],
            Durability.FAST,
        )
        print(message.decode("utf-8"))

        frame = pool.get_lite3(1)
        print(len(frame.payload))

        for item in pool.tail(tags=["note"], max_messages=1, timeout_ms=100):
            print(item.decode("utf-8"))
```

`Pool.tail(..., tags=[...])` uses exact tag matching and composes with other filters using AND semantics.

## Error behavior

- Invalid local arguments raise `ValueError` / `TypeError`.
- ABI/runtime failures raise `PlasmiteError` with `kind`, `path`, `seq`, and `offset` when present.
