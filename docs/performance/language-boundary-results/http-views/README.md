# HTTP owned views

HTTP append now retains the Lite3 owner it builds. The previous client exported
that encoding, freed its owner, then copied and validated the same bytes after
receiving the server's identity. Keeping the owner removes that second acquisition.
Whole-message forwarding also shares the caller's owner. The transport body stays
an independent buffer; failed requests free only newly built, unpublished owners.

Five alternating process pairs use one fixed server, 300 calls per lane and three
repeats per process. Both clients use the same portable Wasm artifact. Tests and
builds stopped during timing. The fixtures contain JSON-safe values, so both
paths request the same application data. Each append returns committed identity
and permits reading the selected field. Setup and complete verification sit
outside timing. See [build fingerprints](environment.json), [raw runs](runs.json)
and client/server sources (outside Git).

## Removing repeated acquisition

These medians include encoding, HTTP, storage, receipt handling and selected-field
access. Combined CPU sums the client and server counters for each measured lane.

| Input | Append before/after µs | Combined CPU before/after µs | Forward before/after µs |
| --- | ---: | ---: | ---: |
| Small | 141.62 / 141.32 | 201.02 / 194.15 | 134.33 / 127.67 |
| Wide | 266.57 / 262.67 | 383.21 / 347.39 | 181.83 / 174.82 |
| 64 KiB string | 212.20 / 208.56 | 321.39 / 321.45 | 197.12 / 187.06 |
| 4,096 objects | 1,922.27 / 1,698.38 | 3,432.57 / 3,232.38 | 882.91 / 692.91 |

The object-array append improves in every pair: 10–15% less elapsed time and
1–12% less combined CPU. Forwarding that array takes 17–23% less elapsed time.
Small and wide append changes overlap run variation; their per-pair CPU changes
also cross zero. Unchanged get lanes show CPU variation despite stable elapsed
time. Treat narrow CPU differences with that limit.

## Comparing complete application paths

The retained client appends and reads one field in 1,698 µs for the object array,
versus 2,763 µs through the JSON API: 39% less elapsed time. Its combined CPU still
costs 3,232 versus 3,066 µs, about 5% more. The Lite3 array occupies 589,952 bytes;
JSON represents these small objects more compactly. Sending fewer transformations
does not guarantee fewer wire bytes or lower CPU.

Wide append takes 263 versus 435 µs through JSON. A 64 KiB string takes 209 versus
318 µs. Small append takes about 141–143 µs through either API. Full native reads
still incur materialization: the array costs 2,364 versus 2,127 µs of combined
CPU through JSON, and the string costs 387 versus 339 µs. Views suit selected reads and
unchanged forwarding; applications that need complete native objects should
measure that requested result.

The new identity response saves the server from formatting an echoed message.
Clients request `application/x-plasmite-identity`; current servers return HTTP 204
with exact sequence/nanosecond headers. Older servers can return their JSON
receipt once; the client reads its exact identity without retrying the append.
Binary point reads require identity headers. Binary tail remains continue-only
and rejects strict terminal-gap requests. JSON tail keeps its existing contract.

## Reproduce

Compile `scripts/process_cpu_macos.c` with `cc -O2 -Wall -Wextra -Werror`.
Build the server and client packages before timing. The raw archive includes
the alternating-pair runner and benchmark harness. Point
`PLASMITE_BENCH_BINDING` at each extracted client, set
`PLASMITE_BENCH_SERVER_PID` and `PLASMITE_SERVER_CPU_PROBE`, then run
`bindings/node/scripts/bench_http_views.js URL OUTPUT 300 3`.

The CPU helper samples the server outside the client's timed region. Client CPU
includes runtime background and garbage-collection work and can exceed elapsed
time when threads run together. These sequential loopback measurements do not
establish network throughput or multi-client capacity.

Historical source snapshots and full build logs stay outside Git. This report
keeps the scripts, summaries and readable results.
