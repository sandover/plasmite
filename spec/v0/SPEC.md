# Plasmite CLI Spec (v0)

This document defines the normative CLI compatibility contract for v0.
It keeps only script-level guarantees; signatures, walkthroughs, and examples live in code docs and `docs/cookbook.md`.

## Scope

- This spec freezes what scripts and automation can rely on.
- This spec does not freeze internal command wiring, help text prose, or implementation structure.

## Versioning + Compatibility

- The CLI surface is versioned as `v0`.
- Within v0, compatibility is additive-only.
- Existing commands, machine-readable flags, and field meanings must not be removed or redefined.
- New commands/flags/fields may be added when existing behavior remains stable.
- Any breaking change requires a new major version and a migration path.

## Stable Surface

### Frozen v0.0.1 Command Set

- `plasmite pool create`
- `plasmite pool info`
- `plasmite pool list`
- `plasmite pool delete`
- `plasmite feed`
- `plasmite fetch`
- `plasmite follow`
- `plasmite version`

`plasmite duplex` is implemented but not frozen in v0.0.1.

### Machine-Readable Interfaces

- Global `--dir` selects the local pool directory.
- Non-streaming commands provide stable machine output via `--json`.
- Streaming reads provide stable JSON Lines via `--format jsonl` or `--jsonl`.
- `feed` append receipts include `seq`, `time`, and `meta` (not echoed `data`).

## Data + Error Contract

### Message Envelope

- Stable message envelope fields: `seq`, `time`, `meta`, `data`.
- `seq` is monotonic per pool.
- `time` is RFC 3339 UTC text in CLI JSON output.
- `meta.tags` is always present (empty array when unset).
- Message workflows are JSON-in/JSON-out.

### Error + Exit Contract

- Errors are emitted on stderr.
- On TTY stderr: concise human text plus actionable guidance.
- On non-TTY stderr: JSON envelope with required `error.kind` and `error.message`.
- Optional error fields may include `error.hint`, `error.path`, `error.seq`, `error.offset`, `error.causes`.
- Exit-code mapping by error kind is stable for v0 and defined by implementation in `src/core/error.rs`.

## Behavioral Semantics

### Pool Reference Resolution

- `NAME` resolves to `POOL_DIR/NAME.plasmite`.
- Explicit paths (for example `./foo.plasmite` or `/abs/foo.plasmite`) are used as-is.
- Resolution rule:
1. If argument contains `/`, treat as path.
2. Else if it ends with `.plasmite`, resolve as `POOL_DIR/<arg>`.
3. Else resolve as `POOL_DIR/<name>.plasmite`.

### Pool Format Compatibility

- Pool files carry an on-disk format version in the header.
- Incompatible on-disk changes must bump format version.
- Older binaries must refuse newer incompatible formats with actionable guidance.

### Platforms

- Supported in v0.0.1: macOS and Linux.

## Non-Contract Surface

The following are implemented but not frozen in v0.0.1 and may evolve within v0:

- `plasmite duplex`
- `plasmite serve`
- `plasmite doctor`
- Remote shorthand refs in CLI commands
- Notice payload details and frequency controls

Current remote shorthand constraints (documented, non-frozen):

- URL refs are explicit remote opt-in in core commands that accept pool refs.
- `duplex` remote refs reject `--create` and `--since`; use `--tail` for remote history.
- `follow` remote refs reject `--since` and `--replay`; use `--tail` for remote history.

## References

- CLI docs of record: `docs/record/vision.md`
- Remote protocol contract: `spec/remote/v0/SPEC.md`
- Public API contract: `spec/api/v0/SPEC.md`
