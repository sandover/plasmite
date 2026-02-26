# Specs

Normative contracts are versioned under `spec/`.

- CLI contract (v0): `spec/v0/SPEC.md`
- Public API contract (v0): `spec/api/v0/SPEC.md`
- Remote protocol contract (v0): `spec/remote/v0/SPEC.md`

## Distillation Rule

Specs capture only externally relied-on compatibility guarantees:

- Versioning and additive/breaking policy.
- Stable surface names and endpoint/command shapes.
- Data, ordering, and error invariants.
- Explicit non-contract surfaces.

Specs intentionally avoid code-level mirroring:

- Language-specific method/function signatures.
- Exhaustive option-by-option docs unless contract-critical.
- Implementation notes, internal architecture details, and long examples.
