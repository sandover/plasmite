<!--
Purpose: Document how to build and use the Plasmite Python bindings.
Exports: N/A (documentation).
Role: Quickstart for Python users of libplasmite.
Invariants: Uses the C ABI via ctypes and matches v0 semantics.
Notes: Requires libplasmite to be built and discoverable.
-->

# Plasmite Python Bindings (v0)

These bindings wrap the `libplasmite` C ABI via `ctypes`.

## Build Requirements

- Python 3.10+
- `libplasmite` built from this repo (`cargo build -p plasmite`)

## Install & Test

From the repo root:

```bash
cargo build -p plasmite
```

Then from `bindings/python`:

```bash
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" python -m pip install -e .
PLASMITE_LIB_DIR="$(pwd)/../../target/debug" PLASMITE_BIN="$(pwd)/../../target/debug/plasmite" python -m unittest
```

On macOS, ensure `DYLD_LIBRARY_PATH` includes the same directory.
On Linux, set `LD_LIBRARY_PATH`.

## Usage

```python
from plasmite import Client, Durability

client = Client("./data")
pool = client.create_pool("docs", 64 * 1024 * 1024)
message = pool.append_json(b'{"kind":"note","text":"hi"}', ["note"], Durability.FAST)
print(message.decode("utf-8"))

frame = pool.get_lite3(1)
print(len(frame.payload))

pool.close()
client.close()
```
