# Review and implementation of the two follow-ups

The working tree now implements **PQ6AEE** and **SPQ3MC**. Opus-5 reviewed both
plans and their source evidence before implementation. Both changes preserve the
existing message formats, error policy and authorization boundaries.

## HTTP error propagation

Eleven ordinary HTTP handlers now return `Result<Response, HttpError>`. The private
wrapper delegates to the existing default error policy. Each handler still owns
its authorization and validation order. Storage admission and worker-side grant
checks keep their existing owners. Streaming, access administration and explicit
status overrides retain their existing paths.

The review found that `ui_list_pools` had the same boilerplate as the ten named
handlers. Including it removes an unexplained exception. The final change removes
63 net production lines from `serve.rs`, including the wrapper and its comment.
Tests add coverage; this count excludes tests. This change claims easier reading,
with no established runtime or memory benefit.

New tests cover malformed JSON before authorization, authorization before invalid
content type or pool reference, origin refusal before body extraction, and revoked
grants under exhausted storage capacity. The last case retains `Busy`/423 because
admission happens before the blocking worker checks the grant.

## Validated metadata without a data tree

`Meta::from_lite3` now owns the SDK tail's existing metadata/readability check.
Local and HTTP binary CLI tag filters use it when no jq predicate needs the data
value. JSON output and jq keep full decoding. UDP keeps its aligned full-message
path. Filtering still checks readability before rejecting a tag or timestamp;
the CLI does not move tag filtering to the server.

The additive public method costs less wiring than compiling shared private source
into both the library and binary crates. A private library method cannot cross
that crate boundary. Hiding a public method would keep its compatibility cost
while making its behavior harder to discover. Its documentation states that it
exports and scans the data as JSON: it saves the value tree, not all data work.

`IgnoredAny` differs from full `Value` parsing for some arbitrary JSON inputs.
Here Lite3 exports the JSON first: its encoder limits nesting to 32, writes finite
numeric values, and rejects invalid Unicode. Those constraints exclude the known
parser differences. The SDK already used this validation sequence. Tests cover
missing/malformed metadata, non-string tags, missing data, unreadable nested data,
excluded tags/timestamps, no data-tree construction and `u64::MAX` timestamps.

The implementation removes the prototype's `expect("binary frame")`, names the
new predicate `needs_data_value`, and adds no custom validation visitor or payload
shape heuristic. Production source grows by 8 lines in `api/message.rs` and 17 in
`cli/stream_support.rs`; tests and documentation add further lines.

## Fresh measurements

These runs compare optimized binaries built from the current pre-change tree
and the implementation, including the concurrent notification-name improvement.
The timing driver checks exact output equality on 24 cases, then collects 25
shuffled, interleaved samples per version per case. The reported run ran after
builds and tests finished. Medians include process startup on this Mac.

| Case | Before | After | Change |
| --- | ---: | ---: | ---: |
| Local, wide structured data, matched tag | 5.513 ms | 4.336 ms | -21.4% |
| Local, nested data, matched tag | 7.613 ms | 6.145 ms | -19.3% |
| HTTP, wide structured data, matched tag | 6.903 ms | 5.800 ms | -16.0% |
| Local, flat 32 KiB string, matched tag | 4.722 ms | 4.464 ms | -5.5% |
| Local, flat string, absent tag | 4.376 ms | 4.273 ms | -2.4% |
| HTTP, flat string, matched tag | 5.387 ms | 5.424 ms | +0.7% |
| HTTP, flat string, absent tag | 5.267 ms | 5.353 ms | +1.6% |

The four flat-string ranges overlap. The earlier five-sample result does not
establish a repeatable 7% regression. These measurements support the structured
workload improvement and no universal speed claim.

A separate temporary Rust allocator probe reproduces the structured savings in
three identical runs per case:

| Local workload | Allocation requests before | After |
| --- | ---: | ---: |
| 64 wide messages, binary tag filter | 36,887 | 599 |
| 64 nested messages, binary tag filter | 59,351 | 599 |
| Wide messages, JSON output | 37,462 | 37,462 |
| Wide messages, binary tag plus jq | 250,126 | 250,126 |

These cumulative request counts exclude C allocation calls and do not measure
peak memory. Requested Rust bytes fall from 3,436,770 to 1,370,413 for wide data
and from 14,414,582 to 5,149,177 for nested data. Small changes since the spike
explain the five additional requests in both versions.

## Review disposition and verification

We accepted Opus's main recommendations: preserve precedence, include the eleventh
ordinary handler, expose and document one metadata reader, remove the prototype
panic, name the predicate by its purpose, and repeat the flat-string measurements.
We kept the two short CLI tag comparisons: one reads JSON values and one reads
strings. A generic helper would add an iterator/type adapter without removing a
validator or data owner.

One review assumption needed correction. Local binary follow without filters
emits stored bytes without inspecting metadata; HTTP's existing SDK tail already
validates them. A black-box test now pins both boundaries, exact local framing,
and corruption errors under excluding tag/time filters.

`cargo fmt --all`, strict all-target Clippy and `just check` pass. Focused suites
also pass: access lifecycle, CLI follow/duplex, remote integration and secure
serving. This work adds no platform-specific decoding or layout assumptions; it
reuses the SDK tail's existing cross-platform check. No native Windows run took
place for these two changes.

The implementation evidence (outside Git) preserves the full
read-only review, sanitized usage metadata, task-local source patch, drivers, raw
allocation/timing results and validation logs. The original spike archive remains
unchanged. Claude reported 54 uncached input tokens, 170,761 cache-write tokens,
3,491,645 cache-read tokens, 27,442 output tokens (17,173 thinking), and **$4.1397525**.
