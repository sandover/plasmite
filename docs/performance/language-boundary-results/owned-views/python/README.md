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

## Native input and complete delivery

`append_view` encodes exact Python builtin input directly into Lite3 and returns
that retained encoding. It removes the outgoing JSON text and its decode pass.
The C stack tracks ancestors for cycle detection, so nested dictionaries do not
create Python tracking objects. The encoding grows without rerunning hooks.
Custom types/subclasses fail explicitly; the existing JSON/dataclass API stays
available. Byte buffers copy into the encoding before storage work releases the
GIL. The compatibility backend snapshots input, then uses bulk JSON for ordinary
data or the checked ctypes builder for byte values and NUL tags.

The next measurements use the newer shared constructor/JSON-capability core,
not the earlier read baseline. Its frozen library hash is
`ea3e5ab7c5bb262a86fcd88140c38a332074157d023fb084a28e0a004f798984`;
core sources (outside Git),
Python/runner sources (outside Git) and
[summary/checks](producer-summary.json) identify the exact artifacts. The
extension uses CPython 3.11.15 headers and the stable 3.11 ABI; runs use 3.15.2.
Seven repeats alternate lane order. Builds/tests stopped during timing.

Median microseconds, new / existing API:

| Requested result | Small | 200 nested fields | 64 KiB string | 1,000 integers |
| --- | ---: | ---: | ---: | ---: |
| Retained encoding / identity-only control | 4.65 / 5.88 | 43.84 / 48.11 | 13.67 / 54.04 | 44.38 / 74.84 |
| Sequence + complete native data | 5.69 / 9.19 | 88.99 / 100.16 | 17.15 / 108.83 | 64.96 / 104.80 |
| Complete JSON string / compact JSON control | 5.70 / 7.16 | 64.82 / 63.92 | 34.02 / 89.30 | 53.84 / 78.67 |

The first row includes additional result ownership on the new path; both
callers can read the committed identity. Native-data and JSON rows compare the
same requested final value. The existing native-data API also creates its
required dataclass, datetime and raw JSON. The compact controls include builtin
JSON serialization and UTF-8 conversion. Wide complete JSON costs about 1%
more on the native path. [Raw native runs](producer-native.raw.json) keep every
sample; [fallback runs](producer-fallback.raw.json) show compatibility costs
separately. A 64 KiB native byte value takes 9.16 µs to append and retain; that
lane has no equivalent JSON value control.

[Native](producer-native.allocations.json) and
[fallback](producer-fallback.allocations.json) allocation diagnostics run in
separate processes with null timings. They exclude native allocations and do
not establish total allocation counts. Retained builder storage can keep spare
capacity beyond the encoded byte length.

### Why faster sending can lower delivery throughput

Five paired runs use independent writer/reader processes. Both writers check
committed identity; the native writer retains and discards its view. The same
JSON consumer decodes and verifies every complete record. These pipeline
fixtures differ from the table above: the small record has 17 metrics, context
and an empty note; wide adds 200 flat integer fields; large adds a 64 KiB note.

| Pipeline | Existing records/s | Native records/s | Combined CPU change |
| --- | ---: | ---: | ---: |
| [Small](producer-pipeline-small.json) | 99,378 | 92,509 | −8.7% |
| [Wide](producer-pipeline-wide.json) | 27,952 | 26,952 | +0.5% |
| [Large](producer-pipeline-large.json) | 14,089 | 14,221 | −33.3% |

The small regression comes from changed file-lock handoffs. Three controls
separate the mechanism from encoder and frame costs:

- [Prefilled readers](producer-prefilled-reader.json), with no concurrent
  writer, consume new/old frames in 8.04/8.13 µs. Encoded frame lengths also match.
- Adding only [1.5 µs of sender work](producer-cadence-1500.json) outside the lock
  raises native delivery from [93k](producer-cadence-0.json) to 107k records/s.
  [3 µs](producer-cadence-3000.json) slows it to 90.5k as the writer becomes the
  limit. Payload, publication and consumer result stay the same.
- A [separate diagnostic](producer-locks-0.json) measures 5.24 ms of writer
  exclusive-lock acquisition and 10.46 ms of reader shared-lock acquisition per
  5,000 records. With [1.5 µs spacing](producer-locks-1500.json), those budgets
  fall to 2.42/6.42 ms. Lock counts remain about 5,000 each. Wake counts remain
  5,000 and wake budgets stay close; the native reader records zero address waits.

Faster encoding makes the writer request the pool lock more often while both
processes run. Their acquisition/handoff cost grows enough to erase the small
encoding gain. These inclusive diagnostic budgets combine syscall execution,
waiting and descheduling. They are separate from ordinary throughput timings;
do not add their medians to reconstruct an uninstrumented operation.
The delay serves only as a diagnostic. The next bounded-batch experiment will
test fewer lock handoffs. Ordinary calls remain unbuffered.

[Probe source](producer-lock-probe.c) targets this macOS host. Compile it with
`cc -dynamiclib -O2` and pass `--native-counts /absolute/path/to/probe.dylib` to
the pipeline runner. Use `--producer-work-ns 1500` for the spacing control and
`--no-pipe --receipt native-view --baseline-receipt identity` for these paired
Plasmite runs. The source archive includes the earlier unpadded pipeline runner
whose hash appears in the first three pipeline files.

### UDP copy removal and final checks

The retained UDP cleanup pins immutable payload bytes instead of building a
ctypes array copy. Mutable inputs still snapshot. Seven independent tracemalloc
samples show Python peak allocation dropping from 8,368 to 4,131 bytes for the
small record, and 270,350 to 200,538 bytes for the 64 KiB string. The removed
buffer and its ctypes array bookkeeping explain this reduction. These
[before](producer-udp-baseline.allocations.json)/
[after](producer-udp-candidate.allocations.json) diagnostics contain null timings
and exclude C/Rust memory. No UDP throughput gain follows from them.

The full Python suite passes 70 tests with one skip. The installed native 3.10
wheel and actual free-threaded 3.14t fallback each pass 18 view/input cases.
Free-threaded Python keeps its GIL disabled. Cases cover cycles, aliases, exact
integers, negative zero, Unicode/NUL rules, rejected hooks, mutable byte input,
growth, canonical timestamps/metadata and blocked-storage thread progress.
The three-wheel log (outside Git) also verifies native input,
byte values and NUL tags in every installed wheel, plus the packaged server.

[Callback ordering checks](callback-guards/README.md) cover reentrant closure during
argument conversion and report the cost of the additional open-handle check.

[Explicit view batches and forwarding](batches/README.md) reduce busy-reader lock
handoffs and reuse immutable encoded input, with matched complete-delivery, paced,
contention and retention measurements.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
