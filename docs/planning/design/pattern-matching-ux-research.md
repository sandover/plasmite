# Pattern Matching UX Research (Pub/Sub)

## Purpose
Survey proven filtering/routing models and extract practical CLI/API design guidance for Plasmite pattern matching.

## Scope
Systems reviewed:
- Redis Streams
- NATS subjects
- RabbitMQ topic exchanges
- jq expression filters
- GraphQL subscriptions
- Kafka topics/partitions
- Oblong Plasma precedent (high-level)

## Redis Streams
### Core model
- Messages are appended to named streams.
- Consumption can be direct (`XREAD`) or group-based (`XREADGROUP`).
- Filtering is mostly by stream key and ID/time window, not deep payload predicates.

### CLI/API UX patterns worth stealing
- Explicit blocking timeout parameters (`BLOCK ms`) map cleanly to CLI timeouts.
- Batch sizing (`COUNT N`) gives easy latency/throughput control.
- Consumer-group naming makes ownership and retries visible.

### Pros
- Operationally simple mental model.
- Strong durability and replay semantics.
- Scales well for many independent streams.

### Cons
- No first-class payload predicate language.
- Topic/stream explosion risk if over-encoding filter dimensions in names.
- Consumer-group complexity can leak into simple use cases.

## NATS
### Core model
- Subject-based routing (`foo.bar.baz`).
- Wildcards: `*` for one token, `>` for remainder.
- Filtering is subject-first; payload filtering is secondary/app-level.

### CLI/API UX patterns worth stealing
- Human-readable wildcard subscriptions (`orders.*.created`).
- Very short syntax for common fan-out/fan-in routing.
- Clear separation between route key and payload.

### Pros
- Extremely fast and intuitive for hierarchical routing keys.
- Easy to reason about common subscription shapes.
- Great CLI ergonomics for ad hoc subscriptions.

### Cons
- Wildcards alone cannot express rich payload conditions.
- Subject taxonomy governance becomes critical.
- Overloaded subject namespaces become brittle.

## RabbitMQ (Topic Exchanges)
### Core model
- Producer sends with routing key; bindings use patterns.
- Wildcards: `*` one word, `#` zero or more words.
- Exchange type determines matching behavior.

### CLI/API UX patterns worth stealing
- Explicit binding key patterns for routing (`audit.*.error`).
- Declarative routing config as composable primitives.
- Clear distinction between exchange, queue, and binding.

### Pros
- Mature routing semantics for event classes.
- Good fit for durable queue topologies.
- Predictable wildcard behavior.

### Cons
- Operational surface area is larger than lightweight local IPC goals.
- Payload-level filtering still usually external/plugin-based.
- Config-heavy workflow can be too much for quick local usage.

## jq (Expression-Based)
### Core model
- Boolean expressions over full JSON messages.
- Can access any field and compose arbitrarily rich logic.

### CLI/API UX patterns worth stealing
- Direct expression flag (`--where '<expr>'`) as an escape hatch.
- Repeatable filters AND-ed together for composability.
- Rich operators without inventing custom mini-language.

### Pros
- Maximum expressiveness.
- Already familiar to JSON tooling users.
- Single mechanism covers metadata and payload.

### Cons
- Verbose for common cases.
- Higher error-rate for quoting and path syntax.
- Performance can degrade for complex expressions if not optimized.

## GraphQL Subscriptions
### Core model
- Client subscribes to typed fields.
- Filtering often happens server-side via arguments and resolver logic.

### CLI/API UX patterns worth stealing
- Typed filter inputs that are self-documenting.
- Strong schema discoverability (introspection/docs).
- Explicit server contract for accepted filters.

### Pros
- Excellent API clarity and discoverability.
- Strongly typed filter contracts.
- Great for frontend/app integration.

### Cons
- Requires schema governance and infra.
- Less lightweight than CLI-first local tooling.
- Query syntax overhead for simple terminal workflows.

## Kafka
### Core model
- Topic-based publish/subscribe with partitioning.
- Routing mainly by topic and partition key.
- Consumer groups coordinate partition ownership.

### CLI/API UX patterns worth stealing
- Clear replay controls (`from beginning`, offsets).
- Partition/consumer-group visibility in tooling.
- Strong operational primitives for backpressure and lag.

### Pros
- Industrial-grade throughput and replay.
- Durable log semantics and robust ecosystem.
- Excellent for large-scale stream processing.

### Cons
- Payload filtering is generally downstream processing, not broker primitive.
- Heavy operational model for local IPC use.
- Topic/key design can become complex quickly.

## Oblong Plasma (high-level precedent)
### Core model
- Shared-memory tuple-space style messaging with structured data patterns.
- Emphasis on low-latency local collaboration and pattern-based retrieval.

### Useful precedent for Plasmite
- Pattern matching can be central, not bolt-on.
- Local-first systems benefit from concise match primitives.
- Strongly suggests balancing expressive matching with low-latency execution.

## Cross-System Synthesis
### Strong patterns
- Keep routing key matching simple and short.
- Preserve a fully expressive escape hatch.
- Make replay/time/window controls explicit and composable.
- Return structured per-filter diagnostics on invalid input.

### Anti-patterns to avoid
- Forcing users to encode all filtering into pool/topic names.
- Introducing a second fully custom query language when jq already exists.
- Hiding matching semantics behind implicit defaults.

## Recommendation for Plasmite
### 1) Keep `--where` as the canonical power tool
- Retain jq-style boolean expressions over full envelope (`seq`, `time`, `meta.tags`, `data`).
- Preserve repeatable `--where` as logical AND.

### 2) Add short, explicit sugar for high-frequency cases
- Introduce additive shortcuts that compile into `--where` semantics:
  - `--tag <value>` (already present; keep first-class)
  - `--field <path=value>` for equality match
  - `--has <path>` for field existence
- Keep output behavior identical regardless of shortcut vs `--where`.

### 3) Add optional route-key style matching for metadata
- Consider `--topic <pattern>` mapped to `meta.tags`/key semantics with wildcard support.
- Limit wildcard semantics to one clear model (NATS-like or Rabbit-like, not both).

### 4) Preserve local/remote parity contract
- Any matching feature added to CLI should map cleanly to remote API and bindings.
- Unsupported combinations must fail with `Usage` + actionable hint.

### 5) Performance strategy
- Fast-path common predicates (`tag equality`, simple field equality) before jq runtime.
- Fall back to jq evaluation for complex expressions.
- Document cost model: cheap predicates vs full expression evaluation.

## Great CLI/API UX examples to emulate
- NATS-style wildcard brevity for route-like filters.
- Redis-style explicit timeout and count controls.
- jq-style single-flag expressive fallback.
- GraphQL-style filter shape documentation (even without GraphQL itself).

## Proposed staged rollout
1. Design/spec phase: define shortcuts as syntactic sugar over `--where`.
2. Spike phase: measure fast-path predicate wins vs pure jq.
3. CLI implementation: add flags + rich help examples.
4. API/bindings implementation: maintain semantic parity and error contracts.

## Concrete examples for Plasmite
- `plasmite peek demo --tag error`
- `plasmite peek demo --field data.level=error`
- `plasmite peek demo --has data.user.id`
- `plasmite peek demo --where '.data.level == "error" and (.meta.tags | index("billing"))'`

## Decision summary
Recommended direction: hybrid model.
- Use concise metadata/field shortcuts for common workflows.
- Keep jq expressions as the universal escape hatch.
- Optimize common cases without fragmenting semantics.
