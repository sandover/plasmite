<!--
Purpose: Explain pattern matching and filtering workflows for `pls peek`.
Key exports: N/A (documentation).
Role: Practical guide for exact tag matching and jq predicate composition.
Invariants: `--tag` is exact/case-sensitive; repeated filters compose with AND.
Invariants: Local and remote `peek` use the same `--tag`/`--where` semantics.
-->

# Pattern Matching Guide (v0)

Pattern matching in Plasmite intentionally has two layers:

- `--tag <value>` for fast exact matches on `meta.tags`
- `--where <jq-expr>` for full-envelope boolean predicates

Use `--tag` for the common path. Use `--where` when you need richer logic.

## Core semantics

- `--tag` is exact, case-sensitive string equality against `meta.tags`.
- Repeat `--tag` to require multiple tags (`AND`).
- Repeat `--where` to require every predicate (`AND`).
- Mix `--tag` and `--where`; they also compose with `AND`.

## CLI recipes

```bash
# Exact tag match
pls peek demo --tail 50 --tag error

# Require both tags
pls peek demo --tail 50 --tag error --tag billing

# Tag prefilter + jq predicate
pls peek demo --tail 50 --tag error --where '.data.service == "payments"'

# Multiple jq predicates (AND)
pls peek demo --tail 50 \
  --where '.data.service == "payments"' \
  --where '.data.latency_ms > 1000'
```

## Local and remote parity

`--tag` and `--where` filter semantics are the same for local and remote `peek`.
Remote `peek` still has its own surface constraints (notably no `--since` or `--replay`).

```bash
# local
pls peek demo --tail 20 --tag error --where '.data.code == 503'

# remote
pls peek http://127.0.0.1:9700/demo --tail 20 --tag error --where '.data.code == 503'
```

## Binding examples

### Go

```go
maxMessages := uint64(10)
out, errs := pool.Tail(ctx, plasmite.TailOptions{
    Tags:        []string{"error", "billing"},
    MaxMessages: &maxMessages,
    Timeout:     100 * time.Millisecond,
})
_ = out
_ = errs
```

### Python

```python
for msg in pool.tail(tags=["error", "billing"], max_messages=10, timeout_ms=100):
    print(msg)
```

### Node (remote)

```js
const tail = await pool.tail({
  tags: ["error", "billing"],
  maxMessages: 10,
  timeoutMs: 100,
});
```

## Choosing `--tag` vs `--where`

- Prefer `--tag` for exact tag membership checks.
- Prefer `--where` for payload math, ranges, or complex boolean logic.
- Combine both for readable and efficient filtering.
