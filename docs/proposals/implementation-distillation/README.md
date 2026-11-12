# Measured implementation distillation

Five investigations compared the current implementation with disposable prototypes
that preserve Plasmite's capabilities. **Pursue two narrow changes and keep the
other three designs.** The proposed larger shared models mostly add code without
removing work or ownership responsibilities.

This study completes Ergo epic **ABMVNH**. The two follow-ups now have an
[Opus review and implementation record](implementation.md). The tables below
retain the original spike measurements.

| Investigation | Recommendation | Measured result |
| --- | --- | --- |
| [Service settings](service-settings.md), 72EGJU | KEEP the current persisted/request model | Complete settings add 47 production lines and 9,008 executable bytes. Serialization rises from 3 to 8 allocation calls. A valid expanded IPv6 address changes its installed-job argument. A smaller extraction saves eight lines but removes no state owner. |
| [Message paths](message-path.md), COP2D4 | SIMPLIFY tag-only binary filtering; keep the existing message representations | The broad model adds 30 lines with unchanged allocations. Metadata/readability reuse adds 22 lines but avoids full data trees: 98–99% fewer Rust allocation requests on tested structured messages. Optimized local wide/nested samples improve 23.6%/17.1%; large flat-string controls slow about 7%. |
| [Browser state](browser-state.md), 5F4SXU | KEEP page-owned progress and the existing shared stream reader | Shared progress adds 16 production JavaScript lines and four mutable state slots. Actual Chrome timing/heap ranges overlap; both versions retain the same bounded rows. A small helper also grows source. |
| [Ordered CLI follow](ordered-follow.md), 346LXD | KEEP the current loops and shared helpers | Shared progress adds 57 lines. Allocation counts remain identical; runtime samples show no consistent gain. One maintenance rule moves from two owners to one within the same file. |
| [HTTP handlers](http-handlers.md), 6SXWY6 | SIMPLIFY ordinary error propagation | A private result-error wrapper removes 61 net production lines across ten handlers. The optimized executable shrinks 672 bytes. Latency/RSS ranges overlap. A larger authorized-request context saves two fewer lines and adds another layer. |

## Implementation follow-ups

- **PQ6AEE — Simplify ordinary HTTP error propagation with Result returns.**
  Keep authorization, extraction order, storage admission, worker-side revocation
  and special HTTP responses explicit. This is a source/readability improvement;
  measurements establish no speed or memory benefit.
- **SPQ3MC — Avoid full data trees in tag-only binary CLI follow.** Reuse one
  metadata/readability implementation for SDK and CLI readers. Preserve corruption
  errors even for excluded messages, exact bytes, JSON/jq behavior and UDP
  alignment. Review the additive Rust accessor and confirm or reduce the measured
  flat-string penalty before acceptance.

These follow-ups affect different production files. The rejected broad message
and ordered-follow models overlap; their source deltas and timings must not be
added together. Neither a universal message model nor a universal stream engine
has earned its complexity in these experiments.

## What changed our judgment

The initial service-settings proposal looked strongest. The prototype showed
that shared normalization already owns most defaults and validation, while raw
listener spelling helps identify an installed job exactly. A new type adds
serialization adapters and field maps before it can remove much existing code.

The message investigation first found little remaining conversion waste, then
identified a specific exception: binary output with only tag filtering still
built the application-data tree. Reading metadata at its existing owner removes
that work. A larger common record would retain it.

The maintenance exercises show modest modularity benefits for shared models,
but those benefits do not automatically produce a smaller or easier-to-trace
implementation. Each report includes the concrete edits, independent review and
cost of the replacement machinery.

## Evidence and limits

Baseline: the current working tree on 2026-11-12, including the completed but
uncommitted 6YYPR5 work and release-note edits. HEAD alone does not contain that
baseline. The study fingerprints 338 files and verifies their exact reconstruction
from the recorded commit, a dirty-tree patch and five untracked source files.
Production inputs remained unchanged throughout the investigations.

The prototype source snapshots, reconstruction patches and full machine logs
stay outside Git. The reports retain the measured results, review decisions
and limits. These historical working-tree experiments require those local
snapshots for exact replay; a clean checkout alone does not reproduce them.

The measurements use one macOS/arm64 host and pinned Rust 1.99.1. CLI model
comparisons use debug binaries; the narrower tag and HTTP studies also include
optimized comparisons. Actual Chrome runs establish only the documented DOM,
retention and heap observations. Rust allocation counters exclude native C
allocations and measure cumulative requests, not peak retained memory. Timing
windows avoid other investigation builds, but results retain ordinary host noise.
No native Windows performance or readiness claim follows from this study.

No full release gate ran for these investigations. Each report names its focused
checks and remaining limits; the implementation tasks require the normal gates
before shipping. Disposable source trees, build caches, foreground servers and
browser profiles are removed after preserving the evidence.
