<!--
Purpose: Iterative design draft for Plasmite pattern matching v0 based on consult decisions.
Key Exports: Normative CLI semantics, API shape guidance, and rollout constraints.
Role: Design contract for spike/implementation tasks before spec finalization.
Invariants: --where remains fully supported; shortcuts are additive and coherent.
Invariants: Local and remote behavior must match for equivalent filters.
Invariants: v0 keeps tag matching exact and deterministic.
-->

# Pattern Matching v0 Iterative Design Draft

## Status
- Draft for review (post-research, pre-implementation).
- Inputs: `docs/design/pattern-matching-ux-research.md` and consult decisions.

## Goals
- Preserve `--where` as the expressive jq escape hatch.
- Make the common case short and obvious with `--tag`.
- Keep semantics identical across local and remote.
- Keep v0 predictable, low-risk, and performant.

## Final Decisions (This Iteration)
1. Shortcut surface: `--tag` only.
2. Composition model: all filters AND together.
3. Tag semantics: exact string membership only.
4. Expression semantics: full jq remains in `--where`.
5. Parity: same UX and behavior for local and remote.
6. Errors: invalid filters are strict `Usage` errors with hints.

## Shortcut Options Explored
### Option A: `--tag` only (selected)
- Example: `plasmite peek demo --tag error`
- Why selected: clear, aligned with recent terminology shift, low complexity.

### Option B: `--tag` + `--field key=value`
- Benefit: common payload equality is shorter.
- Risk: mini-language parsing, escaping edge cases, API parity complexity.
- Decision: defer.

### Option C: `--tag` + `--has key`
- Benefit: useful existence checks.
- Risk: adds feature surface before validating `--tag` usage patterns.
- Decision: defer.

### Option D: wildcard tag patterns
- Benefit: ergonomic for families like `svc.*`.
- Risk: grammar lock-in and future compatibility cost.
- Decision: defer to post-v0.

## CLI Semantics
### Supported filters
- `--tag <value>`: match messages where `meta.tags` contains `<value>` exactly.
- `--where <jq-bool-expr>`: existing jq boolean predicate over full envelope.

### Composition rules
- Repeated `--tag` flags are ANDed.
- Repeated `--where` flags are ANDed.
- `--tag` and `--where` are ANDed together.

### Exact-match behavior
- No glob/regex/wildcards in `--tag` for v0.
- Case-sensitive byte-for-byte equality for tag strings.

### Error behavior
- Invalid/unsupported filter input is `Usage`.
- No silent fallback from invalid predicates.
- Error text must include a hint with a valid example.

## Local/Remote Parity Contract
- Equivalent filters must produce equivalent match sets for local and remote `peek/tail`.
- Unsupported combinations must fail the same way (`Usage` + hint).
- Documentation examples should be runnable against both local and remote where applicable.

## `--where` Extension Strategy
- Preserve current behavior and backward compatibility.
- Keep `--where` as canonical expressive path for metadata + payload logic.
- Treat `--tag` as additive sugar for a common predicate class; do not alter jq semantics.

## Performance Implications
- `--tag` check is low-cost string-membership over `meta.tags`.
- `--where` remains jq-evaluated and therefore relatively higher cost.
- Combined filters should short-circuit cheap checks first (`--tag`) before jq evaluation.
- v0 goal: no regression for existing `--where` users; improved common-case overhead for tag-only filtering.

## CLI Example Matrix (12)
1. `plasmite peek demo --tag error`
2. `plasmite peek demo --tag billing --tag error`
3. `plasmite peek demo --tag error --where '.data.level == "critical"'`
4. `plasmite peek demo --where '.meta.tags | index("error") != null'`
5. `plasmite peek demo --tag prod --tail 50`
6. `plasmite peek demo --tag audit --one`
7. `plasmite peek demo --tag ingest --timeout 10s`
8. `plasmite peek demo --tag error --format jsonl`
9. `plasmite peek http://127.0.0.1:9700/demo --tag error`
10. `plasmite peek http://127.0.0.1:9700/demo --tag error --where '.data.code == 503'`
11. `plasmite peek demo --tag error --where '.data.service == "payments"' --where '.data.latency_ms > 1000'`
12. `plasmite peek demo --where '.meta.tags | any(. == "error" or . == "warn")'`

## API Examples (Go / Python / Node)
### Go
```go
msgs, errs := pool.Tail(ctx, plasmite.TailOptions{
    Tag:   "error",
    Where: []string{".data.service == \"payments\""},
})
for msg := range msgs {
    _ = msg
}
_ = <-errs
```

### Python
```python
for msg in pool.tail(tag="error", where=[".data.level == \"critical\""]):
    print(msg)
```

### Node
```javascript
for await (const msg of pool.tail({
  tag: "error",
  where: ['.data.service == "billing"']
})) {
  console.log(msg)
}
```

Note: exact API field names are illustrative for design; implementation tasks will finalize signatures.

## Risks and Mitigations
- Risk: users expect OR semantics from repeated flags.
- Mitigation: document AND explicitly in help/spec/examples.

- Risk: users expect wildcard tags.
- Mitigation: document exact-match v0 and recommend `--where` for advanced matching.

- Risk: local/remote behavior drift.
- Mitigation: parity tests in implementation tasks.

## Out of Scope (v0)
- `--field`, `--has`, wildcard tag matching.
- Dedicated pattern language beyond jq.
- Non-AND boolean composition flags.

## Handoff to Follow-on Tasks
- `HT7TDL`: review and ratify this draft with Brandon.
- `B3S6UW`: prototype fast-path tag filter + jq fallback behavior.
- `ZKIGZV`: convert this draft into normative spec text.
- `CKEQRP` / `EOQCYX`: implement CLI + API/bindings with parity tests.
