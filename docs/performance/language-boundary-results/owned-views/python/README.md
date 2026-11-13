# Python native views

The native extension makes selected cold reads 2–8 times faster than the
existing complete-message JSON API on these fixtures. It keeps one independent
Lite3 snapshot and crosses the C boundary directly from Python's native
container slots. Reading a child creates a small wrapper; it does not decode
its siblings. Native dictionaries and lists still win repeated lookup and
arithmetic scans after someone has paid to construct them.

## Method and scope

These measurements use CPython 3.15.2 on macOS/Apple M5 Pro. The extension
build uses uv-managed CPython 3.14.6 headers, `Py_LIMITED_API=0x030b0000`,
Apple Clang and `-O3`; the same artifact passes protocol checks under 3.11.
Seven repeats
alternate lane order inside each fixture. Storage pages stay warm; “cold” means
fresh message acquisition. Builds and other agents' tests stopped during timing.
The native and fallback runs use the same frozen span-lookup core library:
`e3f59f98c9f6055f0786b584add6dcfabde43395968993dd0a2bfcee134eeae0`.
The core still performs the older duplicate-normalization pass at acquisition.
Later core constructor changes do not appear in these numbers.

The [native](native.raw.json) and [fallback](fallback.raw.json) files record
all samples, interpreter details and source/library/extension hashes.
Exact measured Python sources (outside Git) preserve the native
extension before later formatting and diagnostic-wording changes. The core source
and binary correspond to the span lookup checkpoint (outside Git).
The common small fixture has three fields, a nested metric and three samples;
wide adds 200 nested objects; string adds 64 KiB; array has 1,000 integers.
Each lane verifies its result outside the timed loop.

## Native extension results

Median times in microseconds:

| Requested result | Small | Wide | 64 KiB string | 1,000-element array |
| --- | ---: | ---: | ---: | ---: |
| Fresh view, selected field | 2.14 | 31.85 | 7.24 | 14.18 |
| Existing get, selected field | 4.32 | 67.44 | 58.96 | 43.70 |
| Fresh view, complete native data | 2.82 | 77.03 | 10.55 | 35.08 |
| Existing get, complete native data | 4.29 | 67.78 | 59.46 | 44.05 |
| Direct JSON from retained view | 0.50 | 21.22 | 19.35 | 9.92 |
| JSON from retained native data | 1.07 | 25.47 | 30.66 | 17.34 |

Warm two-key view access takes 94–107 ns, versus 28–31 ns for an already decoded
dictionary. Summing 1,000 integers through the native iterator takes 19.56 µs;
a native list takes 1.36 µs. The iterator uses a fixed 16-entry buffer and a
bounded C cursor, while the list exposes contiguous PyObject pointers to `sum`.
The C walker builds explicit native copies without transient child wrappers,
but wide cold conversion still costs 14% more than the JSON API. Keep that
regression visible when choosing an API for full-message consumers.

The string copy comparison has different allocation semantics: Python's
`deepcopy` reuses an existing immutable string, while converting encoded Lite3
creates a Python string. Selected views avoid that decoding until requested.

## Fallback and memory

The ctypes fallback preserves lazy immutable containers and readonly buffers.
It pays for extra encoded copies and per-field ctypes calls. Small cold selected
reads take 8.44 µs versus the existing API's 4.30 µs; wide full-native conversion
takes 1.49 ms versus 68.76 µs. It provides runtime compatibility, not native
extension performance. Its direct JSON route still avoids a Python data tree.

Separate [native](native.allocations.json) and
[fallback](fallback.allocations.json) diagnostics report Python tracemalloc
peaks with **null timings**. They exclude C/Rust allocation and mapped storage;
they do not establish total bytes or allocations per operation. Retained-child
and readonly-buffer checks survive pool closure. Encoded byte lengths identify
the snapshot each child retains; peak process RSS does not measure live retained
owner memory. A small child keeps the entire encoded message alive.

## Reproduce

Build the extension, then run each backend separately against the same frozen
library, with no concurrent builds or tests:

```sh
cd bindings/python
uv run --no-project --with setuptools --with wheel python setup.py build_ext --inplace
cd ../..
PLASMITE_LIB_DIR=/path/to/frozen/library PYTHONPATH=bindings/python \
  python3 bindings/python/benchmarks/views.py --output native.raw.json
PLASMITE_PYTHON_VIEWS=0 PLASMITE_LIB_DIR=/path/to/frozen/library PYTHONPATH=bindings/python \
  python3 bindings/python/benchmarks/views.py --output fallback.raw.json
```

Use `--allocations --scale .01 --repeats 1` in separate diagnostic processes.

## Build and runtime checks

[Support checks and exact SDK/wheel hashes](support-checks.json) and the
three-wheel build/install log (outside Git) record the packaging result.
The wheel smoke builds all three variants from the standalone source archive,
checks extension/CLI/library architecture and deployment tags, compares bundled
SDK bytes, installs each wheel and exercises view/native/JSON/buffer lifetime.
The ABI3 installation also passes the bundled secure-server restart smoke.
The full Python suite passes 60 tests with one skip. Actual free-threaded 3.14t
passes eight view tests without importing the extension or enabling the GIL.
CPython 3.10 native wheel and 3.11 ABI3 protocol tests pass. These local checks
cover macOS arm64; existing release CI runs the same wheel smoke on its Linux,
Windows and Intel macOS hosts. Local PyPy execution remains untested.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
