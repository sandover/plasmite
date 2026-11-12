# Ordinary HTTP adapter distillation — 6SXWY6

**Recommendation: SIMPLIFY with ordinary `Result<Response, HttpError>` returns in the ten tested operation handlers. Keep authorization, reference validation and storage admission explicit. Do not add the larger authorized-request context.** Draft implementation follow-up: **PQ6AEE**. The prototype remains outside production source.

The small error wrapper removes 61 net production lines, roughly 21% of the tested handler-and-helper subset. It improves the successful-operation trace without adding mutable state, a dependency or a public contract. Paired optimized measurements establish no speed or retained-memory improvement. The typed context costs more explanation and saves two fewer net lines than the smaller variant.

## Baseline and alternatives

Baseline is the coordinator's current working tree, including the completed but uncommitted6YYPR5 changes and release-note edits. HEAD:`ddcc67fd4db281b101f1c2b099242dfabccd3a79`;338-file manifest SHA256:`350772c5210e7d6b40de821193626d041fa071bd8e87506668d8e11e684af5da`. The source hashes and compact patches accompany this report. A HEAD-only checkout omits the implementation under study.

Current handlers repeat grant/error matching, reference/error matching, worker calls and result/error matching. `StorageExecutor::run_authorized` already owns worker-side revocation checks and bounded admission. Error presentation already belongs to `error_response`; the experiment changes propagation syntax and ownership plumbing, not the error policy.

- **Minimal:** a private 14-line `HttpError(Error)` wrapper implements `From<Error>` and Axum `IntoResponse`. Each converted handler returns a result and uses `?`; it still calls authorize and run_authorized by name.
- **Context:** the same wrapper plus `AuthorizedStorage<'a>` captures AppState/grant and forwards a closure receiving a cloned client. Its combined helper costs 42lines. Construction stays inside each handler, after Axum extraction, so malformed body errors retain their precedence over authorization. A new early authorization extractor would change that precedence; the prototype deliberately avoids it.

Ten real handlers: create_pool, open_pool, pool_info, list_pools, ui_pool_info, delete_pool, append_message, append_lite3, get_message and get_lite3. Lite3 content-type/payload validation and exact output headers remain in the route. MCP, access administration, streaming and status-specific origin checks remain explicit and unchanged.

## Size and shape

All counts use rustfmt output. Helpers count as production cost; moving code receives no savings credit. Test adaptations count separately.

| Attribute | Current | Result-only | Authorized context |
|---|---:|---:|---:|
| Ten handler function lines |295|220|194|
| New helper lines |0|14|42|
| Handlers plus helpers |295|234 (**−61**)|236 (**−59**)|
| Nonblank lines in same subset |293|229|228|
| Whole production portion of serve.rs |2745|2684|2686|
| Explicit match expressions in measured subset |29|0|0|
| New handwritten types |0|1|2|
| New mutable state owners |0|0|0|
| New dependencies, generated code, public migration |0|0|0|
| Release executable bytes |14,826,304|14,825,632 (**−672**)|14,824,864 (**−1,440**)|

The removed match expressions become equivalent `?` propagation branches. They do not establish fewer runtime branches or weaker validation. Two test-only lines add the trait import and normalize a private handler result through IntoResponse before the existing status assertion. Production size excludes those adaptations and all maintenance/probe code.

All optimized binaries have distinct recorded hashes. The first attempted shared-cache build reused a binary; its measurements were discarded before benchmarking. Forced release-package rebuilds produced the binaries in runtime.json. Root-package builds took11.94seconds for the minimal variant and13.46seconds for the context after warm dependencies, while the initial baseline build also rebuilt dependencies. Those one-off, unmatched build times support no compile-speed claim. Manifests and dependency counts do not change.

## Functional and runtime evidence

Both variants pass the existing2 access_lifecycle,28 remote_integration and7 secure_serving tests. The context and minimal versions also pass19 server unit tests covering storage admission, worker/grant revocation, health responsiveness, UI snapshots and related boundaries. No complete release gate was necessary for a disposable prototype.

An additional paired live-server matrix compares status, selected protocol/content headers and normalized bodies across15 cases: JSON list/info/open/get, original large Lite3 bytes and exact headers, missing objects, private UI info, invalid references/content types, malformed JSON, missing remote credentials, and an untrusted Origin. It normalizes only fixture paths, clock-relative age fields and OS file incarnation tokens. Unauthenticated malformed JSON still reports the body error before route authorization; untrusted origin still returns403. All versions agree. This is a selected matrix rather than exhaustive protocol equivalence.

The host runs macOS/arm64 with pinned Rust 1.99.1 and the ordinary release profile. Seven rotated paired rounds use a fresh task-owned foreground server per variant, one storage permit,20warmups, then200keep-alive local HTTP requests per case. All measured servers/processes, test homes, pools and generated test keys remain task-owned and temporary. A private loopback TLS connection checks error precedence; the experiment makes no TLS/trust-performance claim. Other investigation jobs paused CPU-heavy work for the measurement window.

Microseconds/request: median[min–max] of seven batch averages. These are end-to-end Python/client/socket/server/core costs, not isolated handler CPU cycles.

| Case | Current | Result-only | Context |
|---|---:|---:|---:|
| info | 87.25 [80.47–170.94] | 85.27 [81.89–99.49] | 85.32 [83.04–90.83] |
| json64 | 90.67 [79.46–101.09] | 88.38 [83.73–100.51] | 89.37 [87.86–121.39] |
| json16k | 101.25 [98.61–106.02] | 101.94 [90.22–107.23] | 100.05 [99.28–142.88] |
| lite3_16k | 94.59 [91.99–98.75] | 96.57 [83.66–108.58] | 95.07 [85.54–97.19] |
| missing | 62.97 [59.54–64.31] | 62.75 [54.45–64.92] | 62.14 [47.37–63.17] |

The ranges overlap and the direction changes by workload. Report this as runtime-neutral within these samples; do not claim a throughput improvement. A reciprocal of a single-request average is not a concurrent throughput result.

Post-workload RSS medians were9,392/9,376/9,376KiB for current/minimal/context, with overlapping ranges. No per-allocation instrumentation ran for this HTTP study, so no allocation or peak-memory saving follows from RSS. The result wrapper owns the same Error value; source inspection introduces no additional heap owner. Full process/runtime allocation counts remain unmeasured.

Under one deliberately file-lock-blocked admitted read, all 120additional storage requests per variant returned Busy423 and all 30health requests returned200; the held request completed200 after release. Per-request diagnostic busy medians were1.15/1.25/1.38ms and health1.02/1.07/1.18ms under 12client threads. This single burst per variant tests bounded admission and responsiveness; it does not establish saturated throughput or a latency regression/improvement. Existing synchronized unit tests also cover the ownership semantics without scheduling guesses.

## Maintenance exercise and review

Each disposable variant adds the same ordinary read-only pool-bounds operation and then a local-administration-only counterpart. The same compiled test checks a valid remote key on the ordinary operation, refusal of that key on the local-only operation, local allowance and missing-credential refusal. The existing default Permission mapping is401, and the exercise checks that mapping identically. Explicit403 origins remain a separate response boundary. The exercise does not add public routes to the production checkout.

| New function, one production file in each version | Current | Result-only | Context |
|---|---:|---:|---:|
| Ordinary read |25lines|16lines|14lines|
| Read with visible local-admin restriction |30lines|21lines|19lines|

All three maintenance tests pass. Each version still needs two handlers and the same role rule; the minimal version reduces repeated plumbing by nine lines per added handler. The result does not prove fewer future tests or fewer policy owners. The sensitive condition remains visible in the handler, rather than a flag in a generalized dispatcher.

The browser investigator independently reviewed both concrete variants. The review found no authorization/ownership defect: construction preserves extractor/body error order; the helper delegates to existing run_authorized; and Lite3 validation/headers remain visible. It judged the linear success path easier to read but noted that the context merely clones a client and forwards a grant. After helper costs, the smaller result-only variant captures the useful saving.

**Elegance/modularity judgment:** ordinary error propagation makes route operations read linearly, removes repeated response-conversion branches and needs one small private type. Context-based dispatch adds a method/lifetime hop with no net size advantage. Authorization policy, cancellation, permit lifetime and core correctness keep their existing owners. This is a modest, measured local simplification; it does not make the entire server substantially smaller.

## Reproduction and limits

Apply minimal.patch or context.patch to a disposable copy matching the baseline source hashes. Run focused server/unit/remote/access suites, adapting the direct list_pools unit assertion exactly as the patch shows. Compile each optimized binary after forcing a root-package release rebuild in its isolated target (`cargo clean --release -p plasmite`); a cloned cache alone does not prove which snapshot ran. Save binaries under the baseline/minimal/context names expected by http_measure.py. Restore the script's ROOT directory layout or change that one path; run it with loopback/process-inspection permission, then extra_parity.py for the403origin case. The scripts record raw paired samples and hashes in runtime.json. maintenance.py creates isolated exercise copies; maintenance-*.rs and maintenance.json preserve the formatted additions and counts.

Do not compare the initial warm/cold build logs as compile-speed evidence. Native Windows runtime, clean build-time distributions, isolated handler instruction counts, allocation counts and broad saturated throughput remain unmeasured. The follow-up preserves platform-neutral handlers and requires normal repository gates before shipping. No public wire, ABI, disk, package, trust or migration change is proposed.


Evidence filenames identify local experiment files under `reports/http/`, outside Git. The [study index](README.md) explains baseline reconstruction and scope.
