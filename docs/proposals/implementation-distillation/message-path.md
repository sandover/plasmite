# COP2D4 — Remaining internal message-identity savings

**Recommendation: SIMPLIFY the specific binary CLI tag-only path, while KEEPING the current message representations.** A new shared prepared-record model adds 30 lines and eliminates no measured work. A narrower metadata/readability primitive avoids constructing application data trees for local/HTTP binary tag selection: wide/nested Rust allocation requests fall98–99%, with repeatable optimized wide/local and nested/local improvements. That narrow prototype costs 22 production lines, one additive Rust API and4272 optimized executable bytes; flat-string latency is a measured tradeoff requiring confirmation before implementation. Draft implementation follow-up: **SPQ3MC**, described in `followup.md`; do not merge the prototype or claim a universal speedup.

## What we tested

The baseline already shares `FrameRef`, `Message::from_lite3`, canonical `MessageWire`, raw binary receipts and conditional raw-follow decoding. NGYSOW/4W4IUB fixed discarded JSON data-tree construction, CM5R2E/KU2ETS shared wire conversions, and XFIBEH supplied identity-bearing point frames. The suspected large conversion problem no longer exists in these read paths.

The broad prototype introduces `PreparedRecord {seq,timestamp_ns,body}` and `PreparedBody::Json/Encoded`, centralizing local/HTTP conditional decode and output emission. It keeps encoded bytes owned by the record and transfers them without copying. Its JSON constructor preserves optional timestamp parsing: no-since JSON output does not parse text back into nanoseconds. A mandatory exact timestamp for all records would add work to that existing path or change malformed-server error timing; it cannot remove that representation boundary for free. Raw+filter continues to decode exactly once. JSON input/HTTP JSON remains decoded already. Existing feed receipts and point-frame fetch need no change.

The small prototype extracts only `decoded_if_needed` across local, HTTP and UDP CLI record preparation. It leaves their different delivery policies alone. Both are real compiled changes to the current production file; `prototype.patch` and `small-alternative.patch` preserve them.

## Measured size, shape and maintenance

| Attribute | Current | Prepared model | Small helper |
|---|---:|---:|---:|
| Production file physical lines | 725 | 755 (+30) | 723 (-2) |
| Nonblank/non-comment line proxy | 674 | 696 (+22) | 671 (-3) |
| Production files changed | 0 | 1 | 1 |
| New handwritten types | 0 | 2 | 0 |
| New dependencies/generated source/public migration | 0 | 0 | 0 |
| Measured allocation-request change | — | 0 | Not separately instrumented |
| Maintenance: existing function owners changed | 2 | 1 | 2 |
| Maintenance patch additions/deletions, including shared probe | 5/0 | 7/3 | 5/0 |

The matched maintenance exercise adds a private per-successful-output sequence observer without changing public output. The broad model localizes it to `emit`; the baseline and small helper require changes in local and HTTP output owners, plus a saved sequence before the remote record is consumed. All versions edit one file. This is a real modest localization benefit, and the patches preserve successful-write ordering. Six isolated maintenance snapshots (three per spike) apply the provided patches; their compile logs record validation. The change does not demonstrate fewer files or a broad maintenance-time reduction.

The model's enum removes a value/payload-pair spelling, but encoded text output still requires a decoded-value presence invariant, and JSON time still has an unneeded/unknown state (represented by the current0 sentinel). It introduces no new cache or heap ownership. Independent review by the coordinator found no concrete behavior bug, judged the existing helpers already own the important decoding rule, and found no discarded conversion that the new representation removes. The reviewer favored the current code at this size. That is a code judgment, not a numeric elegance score.

## Baseline and method

Baseline: the coordinator's current-working-tree snapshot, including uncommitted 6YYPR5 implementation and release-note edits. HEAD and all 338 file hashes live in `baseline-manifest.json` (SHA256 `350772c5210e7d6b40de821193626d041fa071bd8e87506668d8e11e684af5da`). Only `src/cli/stream_support.rs` differs in the uninstrumented prototypes. Production source size excludes tests, docs and comments as shown separately; no generated source, dependency or public-format change exists. All candidates use pinned rustc 1.99.1 and the debug profile. Each has an isolated target/cache; when reusing a cache for a different snapshot, `cargo clean -p plasmite` forces a debug package rebuild. The measured binaries have distinct SHA256 values recorded in `follow/runtime.json`; build messages named the candidate source paths. No release-size/build-time claims follow from these debug measurements.

`bench_follow.py` creates disposable32MiB pools and one baseline loopback HTTP server. Small/indexed and small/unindexed pools contain512 messages with 24-byte string data; large contains64 messages with 32KiB strings. It runs local/HTTP finite follow in raw, JSON and decoded-filter modes (filter `.data.i >= 0`), using `/dev/null` for output during timings. All11 active workloads produce byte-for-byte identical output from all five binaries before measuring. One warmup precedes seven samples per active workload; a seeded shuffled variant order reduces order bias. Five quiet samples per variant use50ms idle timeout and require exit124. Timings include process launch, CLI parsing, pool opening, filtering/serialization and transport; they do not isolate a single method. They use one Mac host in a coordinator-reserved quiet window. Medians and ranges appear below; raw samples and exact binary hashes are in `follow/runtime.json`. These are bounded debug experiments, not a release throughput claim.

A spike-only identical `GlobalAlloc` counter around `cli::dispatch` measures Rust allocation/reallocation requests and requested bytes, with three samples per case. `allocation-instrumentation.patch`, `add_alloc_probe.py`, `bench_alloc.py` and `allocations.json` preserve the method. It excludes native C mallocs, and requested bytes are cumulative rather than peak/live heap. Instrumented binaries are separate from timing binaries. A single `/usr/bin/time -l` maximum-RSS observation per active workload is diagnostic only; page/layout variation prevents a memory-improvement conclusion. No task-owned server remains running.

Representative Rust allocation results were identical between control and both large models in all 11 workloads. Small raw:530 requests/235444 bytes; small JSON:13329/1694628; small decoded-filter/raw:63561/9025699; large raw:82/2126772; large JSON:1809/19081513. HTTP large JSON had the same2277 requests and a42-byte run-to-run requested-byte variation. Thus these prototypes eliminate no measured Rust allocation requests. `allocations.json` contains all samples.

Functionality: each large prototype passes all 50 existing `cli_follow_duplex` tests. Each small alternative passes the same50 plus all 3 `udp_transport_cli` tests. These suites cover finite selection, filtering, gaps, timeout/output, replay timing, duplex readiness/EOF and transport-specific option rejection. SDK/UDP production policy is unchanged. Exact outputs across the11 measured workloads add a matched baseline oracle. This does not establish native Windows performance, exhaustive cancellation latency or every corruption/u64 edge; changed progress/identity code must retain those conditions under any future implementation. No full release gate was necessary for this isolated investigation.


## Runtime results

Milliseconds: median(min–max). Small=512 messages; large=64 messages/32KiB strings.

| Workload | Control ms | Model ms | Small helper ms |
|---|---:|---:|---:|
| local-small-lite3 | 5.859 (5.52–7.11) | 6.235 (5.70–7.63) | 6.249 (5.71–6.69) |
| local-small-json | 9.766 (8.96–10.04) | 10.039 (9.30–11.31) | 10.936 (9.41–11.31) |
| local-small-lite3-filter | 12.365 (11.83–12.82) | 12.376 (11.84–13.21) | 11.802 (11.71–13.30) |
| local-noindex-lite3 | 4.664 (4.34–4.97) | 4.983 (4.67–5.42) | 4.884 (4.75–5.84) |
| local-large-lite3 | 4.035 (3.86–4.09) | 4.364 (4.23–4.55) | 4.281 (4.03–4.58) |
| local-large-json | 37.761 (35.97–41.86) | 38.648 (36.27–40.82) | 38.817 (35.99–41.98) |
| local-large-lite3-filter | 11.636 (11.40–12.47) | 12.252 (11.93–12.74) | 12.199 (11.68–12.85) |
| http-small-lite3 | 7.469 (7.19–9.21) | 8.155 (7.55–9.64) | 8.267 (7.31–45.64) |
| http-small-json | 10.418 (9.94–12.44) | 10.961 (10.42–11.12) | 10.841 (10.42–11.16) |
| http-small-lite3-filter | 13.974 (12.92–15.10) | 15.190 (14.08–37.45) | 14.677 (13.48–15.99) |
| http-large-json | 39.609 (38.53–46.72) | 39.665 (39.04–43.43) | 40.084 (38.52–42.56) |
| local-quiet-50ms | 59.798 (59.61–62.36) | 59.650 (58.55–64.36) | 62.186 (56.87–65.76) |
| http-quiet-50ms | 63.282 (59.24–64.67) | 67.498 (60.17–67.83) | 66.110 (60.11–67.70) |

Most ranges overlap. The prepared model does not win consistently and several small/raw cases are slower in this debug process benchmark. No release-performance regression claim follows. Diagnostic RSS across the active cases remains approximately 8.7–13.1MiB across all variants, with no consistent saving. Debug binary sizes differ by less than0.04%; debug metadata/layout makes those differences unsuitable as evidence of smaller distribution artifacts.

## Conversion ownership trace and judgment

- Local raw: owned frame payload -> emit original bytes; zero message/data decode, zero timestamp formatting. Both alternatives retain that path.
- Local JSON or filtered raw: frame -> `Message::from_lite3` -> canonical JSON value -> filter/output. The original jq-filter matrix still needs one payload decode and timestamp format; the tag-only exception appears below. The new type does not remove them.
- HTTP raw: framed bytes -> emit; filtered raw adds the same single decode.
- HTTP JSON: deserialize `Message` -> canonical JSON value. Parse the timestamp only when `--since` needs numeric comparison. The wire carries text; a stronger private model must still parse it somewhere.
- Feed/receipt and exact point fetch already retain destination identity and original bytes; they provide no incremental saving for this candidate.

The shared prepared model costs more maintained source and makes correctness depend on another representation. The two-line small-helper saving does not justify a standalone refactor. Keep those representations; the additional tag-only experiment below targets a remaining concrete conversion instead. Revisit if a concrete new operation needs exact identity across more boundaries and a prototype removes actual conversions while improving total source/state counts. This result challenges the earlier design suggestion rather than invalidating the earlier targeted fixes.

## Reproduction and limits

Apply either patch to a copy of the fingerprinted baseline, run `cargo fmt --all`, then `cargo build --bin plasmite --offline` with its own target directory. Run `cargo test --test cli_follow_duplex --offline`; the small helper also needs `--test udp_transport_cli`. Build timing binaries for baseline and all variants, save them under the names used by `bench_follow.py`, then run that script with local socket permission. Allocation scripts create separate copies/binaries. No shipping source was edited.

For the broad model/small-helper comparison, untested/immaterial dimensions are native Windows runtime, optimized binary size/performance, clean build time, and native C allocation accounting. The narrow tag-only alternative has additional optimized measurements below. Dependency and public migration deltas are exactly zero by manifest/API inspection. These limits do not support a claim of broad performance improvement; measured source growth and unchanged work support retaining the current model.

## Additional narrow experiment: binary tag-only filtering

The first matrix tested raw no-filter and jq-filter paths, which prior fixes already handle well. A deeper trace found a remaining unnecessary conversion: `needs_message` includes required tags, so the CLI materializes an entire Message/data Value tree even when binary output needs only metadata tags. The SDK's raw tail already avoids that tree. This revises the initial KEEP conclusion for the narrow case, while leaving the broad-model rejection intact.

The bounded prototype extracts the SDK's existing metadata+readability sequence into `Message::meta_from_lite3`: `decode_meta`, `data_json`, then serde `IgnoredAny`. SDK Lite3Tail delegates to it. Only local/HTTP binary CLI reads without jq predicates use the lighter path. Metadata and data readability are checked before deciding whether tags or timestamps exclude the record. JSON/jq still materialize the full message. UDP retains its existing aligned/full-message path. C export of the data JSON string still occurs; this is not zero serialization.

| Narrow alternative cost/benefit | Result |
|---|---|
| Production handwritten physical lines | +22: +4 library, +18 CLI |
| Production nonblank/noncomment proxy | +19 |
| Changed production files | 2 |
| Additional public surface | 1 additive Rust method in prototype; no existing signature/wire/ABI change |
| New dependencies/types/generator/cache | 0 |
| Optimized executable size | 14826304→14830576 bytes (+4272, +0.029%) |
| Wide local tag-only Rust allocations | 36882→594 requests;3436380→1370022 cumulative requested bytes |
| Nested local tag-only Rust allocations | 59346→594 requests;14414187→5148782 requested bytes |
| JSON/jq allocation behavior | Exactly unchanged counts/bytes in all control cases |
| Maintenance observer exercise | Same two existing output owners as baseline; this is allocation/selection distillation, not emission-state unification |

All24 debug tag-workload outputs and20 optimized outputs match baseline byte-for-byte, including missing-tag empty output. New fixtures directly reject missing/malformed meta, non-string/non-array tags, missing data and unreadable nested data even when the requested tag does not match or the timestamp would exclude the frame. They also assert tag+jq/JSON require full decode and the UDP path remains unchanged. A library decode counter confirms the new accessor constructs zero data trees; full Message decoding increments it once. All215 original library,50 ordered CLI and3 UDP integration tests passed before the three added direct tests; the three direct tests passed separately. The tests remain in `tags-prototype.patch`, separate from the22-line production delta.

Optimized controls use the coordinator's verified baseline binary SHA `68a8c54728a653c2623f399ed666f5786f5d83bfbb6b5f7108bfe02f0f5ca837`. The candidate was built after an isolated `cargo clean --release -p plasmite`, with actual compile path and14.35-second incremental build in `tags-release-build.log`; SHA `581047090435df686cef16ae16947dfaea7b05cd2902ab8ec6f410174bb23d5f`. Dependencies and toolchain match; a copied cache alone did not establish candidate execution. The one observed incremental build time is not a clean-build performance claim.

Inputs: small=512 messages/24-character strings; large=64/32KiB strings; wide=64 messages with 256 string fields each; nested=64 messages with 128 object items each containing an eight-element array. Each pool has tag `bench`; `absent` selects none. `where` means `.data.i >= 0`. The optimized study uses five samples per variant/case after warmup, shuffled variant order, local and loopback HTTP, one reserved quiet host window. Timings include process start. Raw values, maximum-RSS diagnostics, exact inputs and hashes live in `tags-release-runtime.json`; debug samples in `tags-runtime.json`, Rust request counts in `tags-allocations.json`, and scripts preserve commands.

| Optimized workload | Control median ms (range) | Metadata path median ms (range) |
|---|---:|---:|
| local-small-lite3-bench | 6.591 (5.40–7.13) | 6.859 (5.82–7.24) |
| local-small-lite3-absent | 5.176 (4.61–6.22) | 4.494 (4.17–5.07) |
| local-small-json-bench | 5.787 (5.32–5.92) | 6.106 (5.33–6.62) |
| local-small-lite3-bench-where | 6.797 (6.04–7.69) | 7.143 (6.84–7.22) |
| http-small-lite3-bench | 30.642 (19.54–66.24) | 32.506 (19.48–56.37) |
| local-large-lite3-bench | 5.282 (5.23–5.52) | 5.675 (5.44–5.72) |
| local-large-lite3-absent | 5.224 (4.91–5.84) | 5.408 (5.05–6.35) |
| local-large-json-bench | 6.830 (6.28–7.62) | 6.244 (6.00–7.05) |
| local-large-lite3-bench-where | 6.056 (5.61–6.38) | 6.038 (5.66–6.37) |
| http-large-lite3-bench | 6.746 (6.63–7.52) | 7.211 (6.95–7.45) |
| local-wide-lite3-bench | 6.805 (6.46–6.97) | 5.199 (5.13–5.82) |
| local-wide-lite3-absent | 7.038 (6.45–7.20) | 5.590 (4.94–5.93) |
| local-wide-json-bench | 6.807 (6.50–7.23) | 6.807 (6.55–6.99) |
| local-wide-lite3-bench-where | 10.838 (10.20–11.12) | 10.662 (10.30–10.70) |
| http-wide-lite3-bench | 7.450 (7.18–7.59) | 6.459 (6.28–6.57) |
| local-nested-lite3-bench | 8.331 (8.26–8.82) | 6.909 (6.66–7.08) |
| local-nested-lite3-absent | 8.601 (8.09–8.82) | 6.678 (6.29–7.12) |
| local-nested-json-bench | 9.163 (9.11–9.45) | 9.521 (9.06–10.88) |
| local-nested-lite3-bench-where | 15.496 (15.17–15.70) | 15.450 (15.35–15.84) |
| http-nested-lite3-bench | 10.008 (9.86–10.15) | 9.817 (9.71–10.48) |

The optimized wide/local matched case improves6.805→5.199ms (23.6%), unmatched7.038→5.590ms (20.6%), and wide/HTTP7.450→6.459ms (13.3%); ranges separate in those measurements. Nested/local improves8.331→6.909ms (17.1%); nested/HTTP ranges overlap and support no speed claim. Large flat-string tag reads show the important opposite result: local5.282→5.675ms (+0.39ms,7.5%) and HTTP6.746→7.211ms (+0.47ms,6.9%). The data JSON export/scan remains substantial for strings, so removing a Value allocation does not guarantee lower latency. JSON/jq controls mostly overlap; no general whole-product throughput claim follows. Single RSS samples show lower tag-only residency but lack replication for a strong memory-peak claim; cumulative allocation request reductions are the firmer result.

Tradeoff judgment: pursue this narrow reuse because structured tag-only reads shed most Rust allocation requests and improve optimized latency with a small source/API cost. The implementation ticket must remeasure flat controls and avoid material regression before acceptance. Do not add payload-shape heuristics or a lazy universal record model to rescue one benchmark. The same code-quality review favored this concrete primitive because it reuses the authoritative SDK validator and leaves data decoding conditional on actual jq/JSON needs. Prefer an existing/private sharing seam if it avoids the extra supported method without greater wiring cost; measure that choice rather than assuming free API exposure.

The proposal does not reduce repository source size, but it produces a leaner runtime path and a reusable validation boundary. It is distinct from completed4W4IUB, which changed SDK raw tails; this follow-up addresses the remaining CLI tag-only callers. Native Windows validation and broader allocation/latency acceptance remain implementation checks, not completed spike claims. No production source changed.


Evidence filenames identify local experiment files under `reports/messages/`, outside Git. The [study index](README.md) explains baseline reconstruction and scope.
