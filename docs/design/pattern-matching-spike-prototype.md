# Pattern Matching Spike Prototype (`B3S6UW`)

## Goal
Validate the approved direction with a working prototype:
- `peek --tag` exact-match shortcut
- `peek --tag` composes with `--where` via AND
- local/remote parity in behavior

## Prototype Changes
- Added repeatable `--tag <TAG>` to `plasmite peek`.
- Added exact tag matching against `meta.tags`.
- Matching semantics:
  - all `--tag` flags AND together
  - all `--where` flags AND together
  - `--tag` and `--where` AND together
- Applied same filtering logic in local and remote `peek` paths.

## Use Cases Demonstrated
1. Local exact tag filtering:
   - `plasmite peek demo --tail 10 --jsonl --tag keep`
2. Local tag + predicate composition:
   - `plasmite peek demo --tail 10 --jsonl --tag keep --where '.data.level == "error"'`
3. Local multi-predicate composition:
   - `plasmite peek demo --tail 10 --jsonl --tag keep --where '.data.service == "billing"' --where '.data.level == "error"'`
4. Remote exact tag filtering parity:
   - `plasmite peek http://127.0.0.1:9700/demo --tail 10 --jsonl --tag keep`
5. Existing jq-only behavior remains available:
   - `plasmite peek demo --tail 10 --jsonl --where '.meta.tags[]? == "keep"'`
6. Tag-only plus replay for bounded historical scan:
   - `plasmite peek demo --tail 24000 --replay 0 --format jsonl --tag keep`

These are backed by integration tests added in `tests/cli_integration.rs`.

## Performance Measurements
### Setup
- Pool: `tmp/pattern-spike/pools/demo` (32 MiB)
- Dataset: 24,000 messages
  - 12,000 tagged `keep`
  - 12,000 tagged `drop`
- Command family: bounded replay scan (`--tail 24000 --replay 0 --format jsonl`)
- Timing method: `/usr/bin/time -p`, 5 runs per case

### Results
- `tag_only`: `--tag keep`
  - avg: **1.2340s** (min 0.80s, max 1.40s)
- `where_only`: `--where '.meta.tags[]? == "keep"'`
  - avg: **3.0780s** (min 2.90s, max 3.37s)
- `tag_and_where`: `--tag keep --where '.data.service == "billing"'`
  - avg: **2.2560s** (min 2.15s, max 2.56s)

### Interpretation
- Tag shortcut is materially faster than equivalent jq-only filter in this workload.
- Adding jq predicates on top of tag filtering still costs more than tag-only, but remains faster than jq-only tag filtering.
- This supports the chosen model: cheap tag prefilter + expressive `--where` fallback.

## Edge Cases / Gotchas Found
1. `--tail N --one` is "Nth matching record in tail window" behavior, not "first matching record" behavior.
2. With filtering, `--tail` count semantics apply after filtering; large `--tail` can wait longer for sparse matches.
3. Remote tail shape and local tail shape behave consistently for tag filtering in tested scenarios.
4. Missing or malformed `meta.tags` in a message means no tag match (safe exclusion).
5. Repeated `--tag` can over-constrain quickly; docs should emphasize AND semantics.

## Conclusion
Prototype validates the design direction:
- simple `--tag` shortcut is valuable and faster for common workflows,
- `--where` remains essential for complex logic,
- combined behavior is coherent and testable.
