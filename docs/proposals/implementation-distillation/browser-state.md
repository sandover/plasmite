# Browser subscription progress distillation — 5F4SXU

**Recommendation: KEEP the current page-owned progress state and existing shared stream reader. Neither the narrow sequence helper nor a shared progress object earns its added implementation cost today.**

The shared object localizes one hypothetical maintenance change from two production files to one, but it adds 16 production lines, 734 source bytes, four mutable progress slots across the views, and a new way to keep identity out of sync with page metadata. The measured browser workloads show no runtime or retained-memory benefit. No implementation follow-up is recommended.

## Baseline and scope

Baseline is the supplied current-working-tree snapshot, including completed but uncommitted 6YYPR5 and release-note work. HEAD was `ddcc67fd4db281b101f1c2b099242dfabccd3a79`; the whole source manifest SHA256 is `350772c5210e7d6b40de821193626d041fa071bd8e87506668d8e11e684af5da`. Relevant input hashes live in `baseline-inputs.json`. Comparing against HEAD alone would miss the incarnation/replacement implementation this study evaluates.

Read OEPCGK, 2PLTOG and BOG2ST alongside the current architecture/serving/testing docs and the remote/API stream contracts. Prior investigations retained the finite-history composition and the cheap policy harness with explicit no-layout limits. This study tests whether new progress ownership improves the present implementation; it does not reopen paging, exact-ID or framework decisions without evidence.

Production inputs: `ui/common.js`, `ui/pool.html`, `ui/map.html`. Test input: `scripts/ui_smoke.cjs`. The prototype introduces no package, generated code, public endpoint, wire change or distribution dependency.

## Current ownership

The shared `followEvents` reader already owns fetch, SSE parsing, retry wait and cancellation checks. `common.js` already owns exact integer parsing, checkpoint formatting and retained-history start arithmetic.

The pool page owns its full information snapshot, next sequence, replacement generation, latest info request, AbortController, pending/displayed messages, rows and selection. Older-history and point-message requests require its generation/latest-request rules.

The map owns a catalog, selected pool, one peek checkpoint, its stop function, hover/selection and actual message rows. It has no older-history request generation. The catalog also owns the selected pool's incarnation.

The shared part remaining in the two `addMessage` functions is three lines: reject a non-bigint sequence, reject a duplicate/before-checkpoint sequence, then advance. Replacement notices and resets concern different view state. Catalog availability belongs to the catalog views, not an individual subscription.

## Prototypes

1. **Small extraction (`narrow.patch`).** Add `advanceSequence(state, seq)` to existing common.js and call it from both actual `addMessage` paths. No state shape changes.
2. **Shared progress object (`model.patch`).** Add `subscriptionProgress()` with incarnation, nextSeq, generation, infoRequest and observe/reset/invalidate/accept transitions. Integrate it into both actual pool and map paths. Move sequence advancement and incarnation/generation decisions into that object. Rendering, reader lifetime, response generation checks and view resets remain explicit in their owning pages. Existing test assertions retain their scenarios but follow the new state paths and initialize the new identity owner when constructing page state directly.

The shared object adds one progress object and four method functions per selected-view state. The pool keeps three progress scalars before and four after; the map keeps one before and four after. Metadata still carries incarnation too. Reader/timer ownership does not disappear. These are static ownership counts, not byte-level heap estimates.

## Before/after results

| Attribute | Current | Narrow helper | Shared progress model |
|---|---:|---:|---:|
| Production JS lines (common + final inline page scripts) | 2,400 | 2,403 (+3) | 2,416 (+16) |
| Production JS bytes | 118,244 | 118,267 (+23) | 118,978 (+734) |
| Full common/pool/map source bytes | 152,325 | 152,348 | 153,059 |
| Sum of separately gzip-compressed sources | 42,675 | 42,717 (+42) | 42,917 (+242) |
| Lexical `if (` tokens, same JS inputs | 276 | 275 | 276 |
| Existing policy checks passed | 27/27 | 27/27 | 27/27 |
| Existing test file lines | 491 | 491 | 497 (+6 state-adaptation lines) |
| Traced reconnect attempts per view | 16 | 16 | 16 |
| Maximum concurrent retry timers in that trace | 1 | 1 | 1 |
| Retry timers/readers remaining after cancellation | 0 / 0 | 0 / 0 | 0 / 0 |
| Checkpoint after each of 200 replacement cycles with messages 1..2 | 3 | 3 | 3 |
| Retained final pending/map messages in replacement trace | 2 | 2 | 2 |
| Real Chrome retained pool message/DOM rows after 10,000 inputs | 400 / 400 | 400 / 400 | 400 / 400 |
| Real Chrome retained map DOM rows after 10,000 inputs | 200 | 200 | 200 |

The gzip figures are compression proxies; this study does not establish that the server applies gzip to these responses. `if` counts are lexical proxies, not cyclomatic-complexity scores. `size.json` contains per-file sizes/hashes; `resource-traces.json` records the deterministic state/resource traces. No code moved into a new dependency, so the positive source deltas are net additions.

### Browser measurements

Actual task-owned headless Chrome `157.0.8103.78`, V8 `15.7.112.43`, on macOS; Node driver `v26.13.0`. Eight rounds rotate variant order; discard round 0, leaving seven observations per page/variant. Root coordinated a quiet CPU window. Each navigation verifies the requested page identity before measuring. Each page receives the same 10,000 message objects starting at sequence 1. The pool flushes every 100 and retains 400; the map retains 200 actual DOM rows. Pages use production CSS and real message/row functions; map geometry/reset presentation helpers are quieted because no live catalog geometry is supplied. Production boot is suppressed and test exports are injected. This is browser execution of the selected real paths, not live-server/end-to-end authentication or full visual testing.

| 10,000-message burst | Current median [range], ms | Narrow median [range], ms | Model median [range], ms |
|---|---:|---:|---:|
| Pool, real DOM rows | 412.2 [396.4–459.5] | 440.9 [390.7–450.6] | 418.4 [395.0–489.0] |
| Map, real message DOM | 30.5 [29.7–32.5] | 31.1 [29.5–33.6] | 30.8 [29.3–34.5] |

| Post-GC browser heap used | Current median [range], bytes | Narrow median [range], bytes | Model median [range], bytes |
|---|---:|---:|---:|
| Pool | 1,013,968 [1,013,968–1,066,496] | 1,035,104 [1,003,008–1,068,192] | 1,069,640 [1,005,512–1,069,880] |
| Map | 681,448 [668,632–681,448] | 674,456 [670,344–683,160] | 678,396 [672,220–685,052] |

The ranges overlap. These samples support no speedup or memory-saving claim. Heap includes the page and engine state, not just progress objects; no allocation-counter claim follows from it. Inputs are small objects. Real network, font delivery, map geometry, older-history scroll anchoring and platform/browser comparisons remain unmeasured. The existing browser-free harness establishes policy traces, not DOM/layout; only the selected burst retention/heap measurements used actual Chrome.

The full baseline/narrow/model 27-test scenarios cover exact IDs, duplicate filtering, u64 limit, reconnect/resume, cancellation, authorization rejection, finite history, delayed old history, unchanged-incarnation append/retention, replacement and stale-catalog recovery. Additional deterministic traces run 16 failed reconnects then cancellation, 200 replacements with restarted sequences, hidden-view cancellation and a visible-page access recheck. They use recorded timer/request state in the existing VM harness, not wall-clock or browser resource measurements. All variants produce the same outputs.

### Maintenance exercise and independent review

Implemented one disposable new transition: count duplicate messages rejected by each view while retaining the checkpoint. Both baseline and model variants pass all existing 27 tests plus an added shared transition test (28/28).

- Current: modify pool.html and map.html; two production files and +2 net lines.
- Shared model: modify common.js; one production file and +1 net line.

This is a real modularity advantage for adding a shared transition. It is small relative to the model's upfront +16 production lines and extra identity/request state. A view-visible notice would still require the appropriate page rendering edits. The experiment does not demonstrate that all future changes localize equally.

Root independently reviewed the concrete model diff. It found no correctness defect, but noted that generation/infoRequest remain publicly mutable through state.progress; the map receives two counters it does not use; incarnation duplicates the selected pool metadata; and reset/render/abort rules remain in the pages. The sequence-only helper centralizes three identical lines but grows the net implementation. Those findings support the measured tradeoff rather than treating one shared type as automatic elegance.

## Judgment

- **Size:** both variants add source and delivered bytes. Splitting files would not change that result.
- **Performance:** no demonstrated gain; the measured decision work is small compared with actual message/DOM work.
- **Elegance:** the model names a shared concept but makes its boundary incomplete. Public mutable fields and mirrored incarnation require more bookkeeping. Current page transitions remain easy to trace with existing common transport parsing/waits.
- **Modularity:** one future shared transition needs fewer edited files. View-specific resets, history rules and reader APIs still differ. This modest benefit does not pay for the new object today.
- **Correctness/testability:** parity holds in exercised paths. A new API requires fixture adaptation; no current defect or invalid-state elimination was demonstrated. The model does not make invalid assignments impossible.
- **Maintenance/distribution:** no dependencies or public migration, but more production/test code and common asset payload. Executable size and rebuild time were not measured; no claim of savings there.

Keep current ownership and the existing shared followEvents/exact-number helpers. Do not create an implementation ticket for either prototype. Revisit a shared model if a third real view or a repeated cross-view lifecycle defect provides a concrete additional consumer/rule. A larger controller would require another measured prototype; this spike does not prove every possible browser architecture larger.

## Reproduction and artifacts

From a baseline matching `baseline-inputs.json`, apply `narrow.patch` or `model.patch` in separate disposable copies. Run `node --test scripts/ui_smoke.cjs`. Maintenance patches apply on top of their named variants and the report driver adds the same transition assertion to both. The prototype generator and maintenance generator preserve the exact implementation and exercise edits.

`browser-real.mjs` launches installed Chrome with an isolated profile, embeds actual common/page scripts, guards navigation identity, records raw samples and closes only its owned process. Set its root path to the local evidence/snapshot directory, then run `node browser-real.mjs` in a quiet window. `browser-resource.cjs` executes the actual page functions in the existing harness for the resource/visibility/replacement traces. Raw browser samples, summary, resource traces, size table, baseline hashes and compact patches accompany this report. The actual-browser profile was removed after the run. Main production source and Ergo state were never changed by this investigation.


Evidence filenames identify local experiment files under `reports/browser/`, outside Git. The [study index](README.md) explains baseline reconstruction and scope.
