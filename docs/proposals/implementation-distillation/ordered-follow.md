# 346LXD — Shared ordered CLI follow processing

**Recommendation: KEEP the current local and HTTP CLI loops with their existing shared helpers.** A common progress object passes parity tests but grows maintained code by57 lines and leaves transport-specific control flow explicit. A smaller predicate helper changes no noncomment line count. Neither prototype reduces measured allocation requests or yields a consistent runtime improvement. No implementation follow-up is warranted now.

## What we tested

The baseline already shares history selection, the idle-deadline helper, matching rules, `DropReporter`, message decoding and output functions. Local reads additionally own cursor recovery, notify/backoff and playback. HTTP reads additionally own server-tail lifetime/reopening and retention checks after an incomplete finite stream. SDK TailState uses different absolute-deadline/filter/count semantics, while UDP accepts arrival order/duplicates. Those differences remain intact.

The broad prototype introduces `OrderedProgress {expected,upper,finite,drops,deadline}` and `OrderedStep`, sharing initial selection, pre-read stop checks, ordered sequence observation, retention advancement, emitted-output deadline reset and finish. Local playback and transport waits remain in their existing loops. The smaller candidate extracts only the common optional-decoded-message predicate across local/HTTP/UDP, without merging their delivery policies.

## Measured size, shape and maintenance

| Attribute | Current | Ordered progress object | Small predicate helper |
|---|---:|---:|---:|
| Production file physical lines | 725 | 782 (+57) | 726 (+1) |
| Nonblank/non-comment line proxy | 674 | 723 (+49) | 674 (unchanged) |
| Production files changed | 0 | 1 | 1 |
| New handwritten types | 0 | 2 | 0 |
| New dependencies/generated source/public migration | 0 | 0 | 0 |
| Lexical if/match/loop/while sites in file (rough proxy) | 85 | 82 | Not a decision criterion |
| Measured allocation-request change | — | 0 | Not separately instrumented |
| Maintenance: existing function owners changed | 2 | 1 | 2 |
| Maintenance additions/deletions including common observer | 4/0 | 3/0 | 4/0 |

The paired maintenance exercise adds a private observer when an ordered reader discards a sequence below its checkpoint. The baseline and small helper require two call sites in two existing functions; `OrderedProgress::observe` requires one. All three edit one production file. The included patches were actually applied to isolated snapshots and compile-checked. The one-call-site advantage is real; it does not repay57 maintained lines on the current two loops by itself. Existing shared matching helpers already localize an ordinary predicate change to one function before this refactor.

The object groups five values in one place, but the loops continue to inspect/update its expected sequence, drop reporter and deadline for different protocol/replay/EOF cases. Hiding those accesses would require more methods or a larger event protocol. The extra enum and cross-method control tracing therefore add a cost even though the state declaration looks smaller at each call site. Independent coordinator review found no concrete behavior bug and judged the existing flat loops easier to follow at their current size. The small predicate is a reasonable optional style preference, not substantial distillation.

## Baseline and method

Baseline: the coordinator's current-working-tree snapshot, including uncommitted 6YYPR5 implementation and release-note edits. HEAD and all 338 file hashes live in `baseline-manifest.json` (SHA256 `350772c5210e7d6b40de821193626d041fa071bd8e87506668d8e11e684af5da`). Only `src/cli/stream_support.rs` differs in the uninstrumented prototypes. Production source size excludes tests, docs and comments as shown separately; no generated source, dependency or public-format change exists. All candidates use pinned rustc 1.99.1 and the debug profile. Each has an isolated target/cache; when reusing a cache for a different snapshot, `cargo clean -p plasmite` forces a debug package rebuild. The measured binaries have distinct SHA256 values recorded in `follow/runtime.json`; build messages named the candidate source paths. No release-size/build-time claims follow from these debug measurements.

`bench_follow.py` creates disposable32MiB pools and one baseline loopback HTTP server. Small/indexed and small/unindexed pools contain512 messages with 24-byte string data; large contains64 messages with 32KiB strings. It runs local/HTTP finite follow in raw, JSON and decoded-filter modes (filter `.data.i >= 0`), using `/dev/null` for output during timings. All11 active workloads produce byte-for-byte identical output from all five binaries before measuring. One warmup precedes seven samples per active workload; a seeded shuffled variant order reduces order bias. Five quiet samples per variant use50ms idle timeout and require exit124. Timings include process launch, CLI parsing, pool opening, filtering/serialization and transport; they do not isolate a single method. They use one Mac host in a coordinator-reserved quiet window. Medians and ranges appear below; raw samples and exact binary hashes are in `follow/runtime.json`. These are bounded debug experiments, not a release throughput claim.

A spike-only identical `GlobalAlloc` counter around `cli::dispatch` measures Rust allocation/reallocation requests and requested bytes, with three samples per case. `allocation-instrumentation.patch`, `add_alloc_probe.py`, `bench_alloc.py` and `allocations.json` preserve the method. It excludes native C mallocs, and requested bytes are cumulative rather than peak/live heap. Instrumented binaries are separate from timing binaries. A single `/usr/bin/time -l` maximum-RSS observation per active workload is diagnostic only; page/layout variation prevents a memory-improvement conclusion. No task-owned server remains running.

Representative Rust allocation results were identical between control and both large models in all 11 workloads. Small raw:530 requests/235444 bytes; small JSON:13329/1694628; small decoded-filter/raw:63561/9025699; large raw:82/2126772; large JSON:1809/19081513. HTTP large JSON had the same2277 requests and a42-byte run-to-run requested-byte variation. Thus these prototypes eliminate no measured Rust allocation requests. `allocations.json` contains all samples.

Functionality: each large prototype passes all 50 existing `cli_follow_duplex` tests. Each small alternative passes the same50 plus all 3 `udp_transport_cli` tests. These suites cover finite selection, filtering, gaps, timeout/output, replay timing, duplex readiness/EOF and transport-specific option rejection. SDK/UDP production policy is unchanged. Exact outputs across the11 measured workloads add a matched baseline oracle. This does not establish native Windows performance, exhaustive cancellation latency or every corruption/u64 edge; changed progress/identity code must retain those conditions under any future implementation. No full release gate was necessary for this isolated investigation.


## Runtime results

Milliseconds: median(min–max).

| Workload | Control ms | Model ms | Small helper ms |
|---|---:|---:|---:|
| local-small-lite3 | 5.859 (5.52–7.11) | 6.372 (5.63–7.23) | 6.035 (5.69–6.68) |
| local-small-json | 9.766 (8.96–10.04) | 10.364 (9.55–12.36) | 10.607 (9.66–13.13) |
| local-small-lite3-filter | 12.365 (11.83–12.82) | 12.271 (11.99–13.17) | 12.664 (11.78–13.17) |
| local-noindex-lite3 | 4.664 (4.34–4.97) | 5.110 (4.81–5.37) | 5.084 (4.77–5.73) |
| local-large-lite3 | 4.035 (3.86–4.09) | 4.243 (3.97–4.53) | 4.150 (3.86–4.40) |
| local-large-json | 37.761 (35.97–41.86) | 39.030 (36.40–41.52) | 37.948 (36.06–40.42) |
| local-large-lite3-filter | 11.636 (11.40–12.47) | 12.316 (11.85–12.57) | 12.189 (11.62–13.50) |
| http-small-lite3 | 7.469 (7.19–9.21) | 8.097 (7.57–9.24) | 7.999 (7.40–8.34) |
| http-small-json | 10.418 (9.94–12.44) | 10.861 (10.49–12.08) | 10.992 (10.61–11.38) |
| http-small-lite3-filter | 13.974 (12.92–15.10) | 13.850 (13.36–15.77) | 14.983 (14.05–15.91) |
| http-large-json | 39.609 (38.53–46.72) | 39.487 (39.09–40.80) | 39.978 (38.94–40.48) |
| local-quiet-50ms | 59.798 (59.61–62.36) | 58.919 (57.43–62.81) | 61.752 (57.22–63.92) |
| http-quiet-50ms | 63.282 (59.24–64.67) | 64.935 (61.64–67.71) | 64.335 (61.51–65.60) |

No candidate wins consistently. Existing tests cover replay interruption/timeout and duplex receive-before-send/EOF; the benchmark adds matching quiet idle-timeout results. We did not measure OS scheduler wake counts or a separate cancellation-latency distribution, so no CPU-wakeup/cancellation improvement follows. Allocation requests match control exactly in all 11 cases; the object only moves existing stack/local ownership.

## Control-flow comparison and overlap

Before: read/receive -> bounds/duplicate check -> gap/checkpoint -> decode-if-needed -> match -> optional local playback -> emit -> idle timer/one-message termination -> transport wait/reopen. Most matching/output operations already call shared named helpers.

After: read/receive -> `progress.observe` -> decode-if-needed -> match -> optional local playback -> emit -> `progress.emitted` -> transport wait/reopen, with the loop still reaching `progress.expected`, `.drops` and `.deadline` for transport-specific decisions. This trades a few short repeated blocks for an additional state type, outcome enum and method jumps. It does not collapse the overall loop architecture.

Both this spike and COP2D4 edit the same read/preparation/emission region. Their standalone source deltas and runtime observations must not be summed. We did not build a combined state/record model because neither model establishes a worthwhile saving; combining them would need another measured control rather than arithmetic on line counts. COP2D4 later found a separate useful metadata-only tag-filter optimization. That can retain these existing loops and does not justify OrderedProgress; do not add its workload-specific runtime gain to these state-model measurements.

## Reproduction and judgment

Apply `prototype.patch` or `small-alternative.patch` to a copy of the common baseline, use an isolated target and run `cargo build --bin plasmite --offline` plus `cargo test --test cli_follow_duplex --offline`; run UDP CLI tests for the small cross-transport helper. Benchmark and allocation scripts are shared with COP2D4 and preserve exact commands, input sizes, binary/source hashes and raw samples. Rebuild a package when changing snapshots; a copied Cargo cache alone does not prove the candidate executed.

Retain explicit local/HTTP/UDP/SDK boundaries. Revisit shared ordered progress if a third ordered CLI transport or a real rule change makes coordinated edits larger, or if a narrower helper removes a measured chunk of processing without another state protocol. No public behavior, ABI, disk or deployment migration is justified by these experiments. Native Windows and optimized release performance remain unmeasured; source/dependency/migration costs and the measured debug paths support KEEP.


Evidence filenames identify local experiment files under `reports/follow/`, outside Git. The [study index](README.md) explains baseline reconstruction and scope.
