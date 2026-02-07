<!--
Purpose: Define Plasmite documentation taxonomy and provide a canonical index.
Exports: N/A (documentation).
Role: Docs entrypoint describing where planning/design docs live vs docs of record.
Invariants: Normative contracts remain in `spec/`; docs-of-record live in `docs/record/` and `docs/decisions/`.
-->

# Docs

Plasmite docs are split into two categories:

- `docs/record/`: canonical docs of record (current operational/reference truth)
- `docs/planning/`: in-progress strategy, proposals, design drafts, and spikes

`docs/decisions/` (ADRs) are also docs of record.

## Promotion lifecycle

1. Draft in `docs/planning/`.
2. Review and accept.
3. Promote accepted content into `docs/record/` (or `docs/decisions/` for ADRs).
4. Keep temporary stubs at old paths when needed for link compatibility.

## Start here

- Docs of record index: `docs/record/README.md`
- Planning/design index: `docs/planning/README.md`
- Decisions (ADRs): `docs/decisions/README.md`
- Taxonomy migration map: `docs/planning/docs-taxonomy-migration-map.md`

## Normative specs

- v0 CLI + message contract: `spec/v0/SPEC.md`
- v0 public API contract: `spec/api/v0/SPEC.md`
- v0 remote protocol: `spec/remote/v0/SPEC.md`

## Key docs of record

- Architecture: `docs/record/architecture.md`
- Rust API quickstart: `docs/record/api-quickstart.md`
- Go quickstart: `docs/record/go-quickstart.md`
- Exit codes: `docs/record/exit-codes.md`
- Diagnostics (`doctor`): `docs/record/doctor.md`
- Testing: `docs/record/TESTING.md`
- Releasing: `docs/record/releasing.md`
- Homebrew packaging: `docs/record/homebrew.md`
- C ABI guide: `docs/record/libplasmite.md`

## Language bindings

Official bindings for embedding Plasmite without the CLI:

| Language | Location | Notes |
|----------|----------|-------|
| Go | `bindings/go/` | cgo wrapper over libplasmite |
| Python | `bindings/python/` | ctypes wrapper over libplasmite |
| Node.js | `bindings/node/` | N-API wrapper; includes `RemoteClient` |

All bindings use the C ABI (`libplasmite`). See `docs/record/libplasmite.md`.

## Planning highlights

- Vision: `docs/planning/vision.md`
- Roadmap: `docs/planning/roadmap.md`
- Performance baselines and investigations: `docs/planning/perf.md`
- Design drafts and spikes: `docs/planning/design/`
