# Service settings distillation: KEEP the current persisted model

**Decision:** Keep the current request/saved-settings representation. The complete-settings prototype makes some callers easier to read, but adds 47 production lines and wire adapters, provides no measured speed benefit, and changes valid existing job arguments. The smaller listener-parsing extraction saves eight lines; that saving does not justify a new internal interface by itself. Create no implementation follow-up from this spike.

## What the current code already owns

`serve_service::effective_args` owns defaults, origin aliases, numeric parsing and static validation. `merge` owns update omissions and explicit UDP disable flags. `Setup` keeps the normalized CLI-shaped values that generated an installed job. Unix checks the saved record's required fields before operation. Windows authenticates its protected installation record, account and native service configuration, then normalizes again when starting the service.

The initial design sketch overstated duplicated completeness checking: Windows has no matching Unix-style required-field block. Most defaults and validation already have one shared owner. Native identity, permissions, staging, rollback and recovery work account for much of the two platform modules; changing settings types does not remove that work.

## Prototypes

1. **Complete settings:** `Settings` replaces `Setup.run: ServeRunArgs`. Local/remote listeners become required `SocketAddr` values, UDP becomes `Option<SocketAddr>`, and numeric limits become required integers. Foreground serving, Unix argv/preflight/status and Windows status/startup consume this value. Serde `try_from`/`into` adapters keep ordinary setup JSON field names. CLI request and patch handling still use `ServeRunArgs`. This is a bounded complete-value prototype, not a separate new patch framework.
2. **Small extraction:** `listener_addresses` parses the already-validated local/remote/UDP addresses for foreground serving and fixed-port checks, preserving the saved representation and exact original spellings.

`typed-settings.patch` and `narrow-listeners.patch` contain the actual changes, including adaptations to existing tests. Neither touches the main checkout. `source-metrics.json` supplies per-file counts and hashes.

## Evidence

Baseline is the current working-tree snapshot supplied by the coordinator, including the uncommitted ten implementation outcomes and release-note edits. It is **not** a checkout of HEAD alone. HEAD is `ddcc67fd4db281b101f1c2b099242dfabccd3a79`; baseline manifest SHA-256 is `350772c5210e7d6b40de821193626d041fa071bd8e87506668d8e11e684af5da`.

Host: macOS 27.2 (26B123), arm64. Toolchain: rustc 1.99.1 (`58cf8f9c3`), LLVM 23.1.1. Measurements use the same host and Cargo release profile. No new dependency, generator, package channel or service registration was introduced. Both CLI/test executables have distinct recorded SHA-256 values; builds reported compilation from the intended source copies. Later probe builds explicitly used `cargo clean --release -p plasmite` in the task-owned target directory.

| Attribute | Current | Complete settings prototype | Small extraction |
|---|---|---|---|
| Production physical lines across the five affected files | 3,908 | 3,955: **47 more** | **8 fewer** across its two files |
| Production nonblank lines | — | **43 more** | **9 fewer** |
| Generated code / dependencies | None added | None added | None added |
| Adapted existing test lines | — | Six more physical lines in Unix service tests | No test edits |
| macOS release CLI bytes | 14,826,304 | 14,835,312: **9,008 more**, about 0.061% | Not measured; no distribution claim |
| Settings stack size | 248 bytes | 224 bytes: 24 fewer | Same representation |
| Serialization allocator calls per operation | 3 | 8 | Not measured; same saved representation |
| Serialization requested allocation bytes per operation | 896 | 974 | Not measured |
| JSON-load allocator calls / requested bytes | 7 / 95 | 7 / 95 | Not measured |
| Existing focused Unix service tests | Baseline already covered by the coordinator's passing current-tree gate | 26 passed | 26 passed |
| Explicit legacy IPv6 job-argument parity test | Pass | **Fail** | Exact spellings remain in saved strings |
| Matched maintenance edit | Two production lines in one file | Same two lines in one file | Same shared owner; not separately exercised |

Line counts exclude the `mod tests` bodies and appended spike probes; physical totals retain the same tiny test-only import annotations in both versions. Generated compiler expansion is not counted as handwritten source. No line reduction comes from moving files or excluding adapters.

### Runtime measurements

The normal release probe uses the actual `effective_args`, `serde_json::from_str::<Setup>` and `Setup::argv` functions with `black_box`. Four process runs use ABBA ordering: baseline, candidate, candidate, baseline. Each process takes eleven samples of 20,000 operations, giving 22 samples per operation and variant. The coordinator paused other spike builds/timings during the 0.86-second sample window. `samples.json` contains every sample; probe stdout is retained in four small logs.

| Operation | Current median, range (ns/op) | Typed median, range (ns/op) |
|---|---:|---:|
| Resolve default configuration | 63.70; 56.35–142.84 | 75.44; 70.94–82.11 |
| Load a saved setup JSON record | 357.83; 336.52–689.47 | 363.91; 358.50–373.21 |
| Construct service argv | 306.39; 265.31–464.60 | 370.92; 349.55–398.14 |

This shows no speed advantage for this prototype. The overlapping ranges and small absolute costs do not establish a reliable whole-process startup regression or a meaningful application-level slowdown. These configuration operations run on service setup/start/status paths, not per message. Filesystem, certificate, elevation and native-manager costs remain outside the probe.

A separate test-only `GlobalAlloc` counter measures 1,000 serialization/load operations with a configured public server origin. It runs independently of the timing probe. Serde `into` clones `Settings`, then creates the CLI-shaped serialization value and formats typed addresses. That adds five allocation/reallocation calls and 78 requested bytes per serialization. Stack-size reduction does not establish lower peak heap or resident memory; those were not measured.

## Correctness, elegance and modularity

Callers do improve: foreground serving reads typed addresses and limits directly, and the Unix loader loses its twenty-line completeness block. Exact required-value presence and address-syntax mistakes cannot inhabit the corresponding typed fields.

The prototype does **not** eliminate every invalid state. Its raw-to-settings adapter still permits zero limits, a non-loopback local listener, a bad public origin, mismatched TLS fields or UDP-follow without UDP when decoding a saved record. The existing CLI normalizer rejects or resolves those cases. A fully validated saved type would need to centralize those semantics as well, while preserving when filesystem and authority checks occur. The Windows direct-start path would need particular review before any production use.

Two real compatibility observations prevent treating the prototype as a ready refactor:

- A valid saved `[0:0:0:0:0:0:0:1]:9700` listener becomes `[::1]:9700` when `SocketAddr` formats it. The baseline preserves the original text. The added parity test passes the baseline and fails the prototype. Unix job definitions come from `Setup::argv`, and ownership checks compare the exact definition; preserving semantic address equality alone is insufficient. A production migration would need the old spellings or additional raw/parsed state.
- Removing a required numeric member from serialized setup succeeds at baseline Serde decoding but fails typed decoding. The existing Unix loader rejects incompleteness later; Windows follows a different load/start path. Moving the rejection changes error timing and needs deliberate compatibility handling.

The type also adds two manually synchronized field maps around the existing CLI declaration. That is extra maintenance ownership even though individual consumers read fewer `unwrap`/parse operations. A dedicated patch type, stronger TLS/limit types and compatibility adapters would require further work not credited as already solved here.

The coordinator independently reviewed the concrete code. The review matched the adapter allocation cost, partial rather than complete validity guarantees, and raw-spelling/native-job compatibility concern. The review found no reason to boot Windows merely to reject this candidate on the valid-input parity failure already demonstrated on macOS. Windows code was edited for the prototype but not compiled or executed; no Windows readiness claim follows from this spike.

## Matched maintenance exercise

Add a synthetic upper bound of 3,600,000 ms to the existing max-tail-timeout rule, with matching error text. Both variants change exactly two production lines at the shared `effective_args` site in one file. The same test checks zero, one, the ceiling and one above it, and passes both versions. No persistence, backend or runtime edit is needed in either variant. This exercise demonstrates that the relevant validation ownership was already centralized. It is a disposable edit and does not recommend a product timeout-policy change.

The patches and `maintenance-probe.rs` preserve the exercise. The prototype's exact-spelling parity failure is retained separately in `compat-candidate.log`, with exit status 101; this is the expected failed experiment, not a passing equivalence claim.

## Reproduction and disposition

Copy the coordinator's working-tree baseline, apply `typed-settings.patch` or `narrow-listeners.patch`, and use a task-owned Cargo target directory. Do not mutate the production checkout or clear its debug artifacts.

```sh
cargo test --bin plasmite serve_service:: --target-dir "$TASK_TARGET"
cargo build --release --bin plasmite --target-dir "$TASK_TARGET"
# Append probe.rs to src/serve_service.rs only in the disposable copies.
cargo clean --release -p plasmite --target-dir "$TASK_TARGET"
cargo test --release --bin plasmite distillation_probe --no-run --target-dir "$TASK_TARGET"
# Save each emitted test executable, then run the two saved binaries in ABBA order:
./saved-probe --exact serve_service::distillation_probe::contract_and_perf --nocapture --test-threads=1
# Append allocation-probe.rs separately for deterministic allocation counts:
cargo test --release --bin plasmite settings_allocation_probe --target-dir "$TASK_TARGET" -- --nocapture --test-threads=1
```

For the maintenance exercise, apply the corresponding maintenance patch and append `maintenance-probe.rs`; run each named test separately. Strip the spike-only probes when counting production code. `summary.json` records executable hashes, raw size totals, timing summaries and allocation results.

**KEEP** is a scoped recommendation based on these prototypes. Revisit if a future configuration redesign can remove raw/typed synchronization while preserving exact installed-job identity, or if new concrete settings work demonstrates repeated maintenance beyond the current shared normalizer. Neither a broad type migration nor an eight-line parse helper has enough measured benefit to justify an implementation ticket now. All operating-system authority, permission and rollback checks remain required.


Evidence filenames identify local experiment files under `reports/services/`, outside Git. The [study index](README.md) explains baseline reconstruction and scope.
