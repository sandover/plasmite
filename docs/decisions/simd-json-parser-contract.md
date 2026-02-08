<!--
Purpose: Define the parser behavior contract for the simd-json-only runtime transition.
Exports: Canonical parsing policies, error taxonomy, and sample input expectations.
Role: Decision record used by implementation and test authors before parser refactors.
Invariants: Parser behavior must remain stable against this contract unless superseded.
Invariants: Error categories stay stable even if wording evolves.
Notes: Consult Me captures policy decisions that need product-level confirmation.
-->

# Decision Record: simd-json parser contract

- Date: 2026-02-08
- Status: Proposed

## Scope

This contract defines expected parse outcomes for JSON payload ingestion when
`simd-json` is the only runtime parser. It is the baseline for behavior, tests,
and migration reviews.

## Parser Contract

### Duplicate keys

- Default policy: reject objects with duplicate keys as invalid input.
- Rationale: strict reject prevents silent data shadowing and keeps input intent
  explicit.
- Category mapping: `duplicate_key`.

### Large and precise numbers

- Integer range accepted: signed 64-bit (`-9223372036854775808` to
  `9223372036854775807`).
- Unsigned values larger than signed range are rejected unless explicitly parsed
  through a string-encoded schema field.
- Floating-point values are accepted as IEEE-754 `f64`; NaN/Inf literals are
  rejected because they are not valid JSON.
- Category mapping: `number_out_of_range` or `number_not_representable`.

### UTF-8 handling

- Input bytes must be valid UTF-8.
- Invalid UTF-8 sequences are rejected and never repaired or replaced.
- Unicode escape sequences are accepted only when they decode to valid Unicode
  scalar values.
- Category mapping: `invalid_utf8`.

### Depth and size limits

- Maximum nesting depth: 128 levels.
- Maximum payload size: 8 MiB for a single decode operation.
- Values exceeding either limit are rejected deterministically.
- Category mapping: `depth_limit_exceeded` or `size_limit_exceeded`.

### Error taxonomy

Stable parser error categories:

| Category | Meaning | Retry guidance |
| --- | --- | --- |
| `syntax_error` | JSON grammar violation | Fix payload shape |
| `invalid_utf8` | Input is not valid UTF-8 | Re-encode as UTF-8 |
| `duplicate_key` | Object repeats a key | Remove ambiguity |
| `number_out_of_range` | Integer cannot fit contract range | Clamp or encode as string |
| `number_not_representable` | Numeric literal cannot be represented safely | Use schema-compatible form |
| `depth_limit_exceeded` | Nesting depth exceeds 128 | Flatten payload |
| `size_limit_exceeded` | Payload exceeds 8 MiB limit | Chunk or reduce payload |

## Canonical Sample Inputs and expected outcomes

| Case | Input snippet | Expected outcome | Error category |
| --- | --- | --- | --- |
| Valid object | `{"a":1,"b":"ok"}` | Parse succeeds | N/A |
| Duplicate keys | `{"a":1,"a":2}` | Parse fails | `duplicate_key` |
| Invalid UTF-8 bytes | raw bytes `FF FE` | Parse fails | `invalid_utf8` |
| Deep nesting | 129 nested arrays | Parse fails | `depth_limit_exceeded` |
| Oversized payload | JSON document > 8 MiB | Parse fails | `size_limit_exceeded` |
| Huge integer | `{"n":9223372036854775808}` | Parse fails | `number_out_of_range` |
| Non-finite token | `{"n":NaN}` | Parse fails | `syntax_error` |

## Consult Me

1. Duplicate-key policy tie-break: keep strict reject, or use compatibility mode
   (`last-key-wins`) for selected legacy input paths.
2. User-facing error wording: keep previous message text where feasible, or allow
   revised wording while preserving stable error categories listed above.
