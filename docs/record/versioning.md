<!--
Purpose: Define lockstep version policy across CLI and language bindings.
Key exports: Compatibility rules, mapping table, and release invariants.
Role: Source of truth for maintainers and automation scripts.
Invariants: Cargo/Python/Node published artifacts share one exact version.
Notes: Update this doc if ABI policy or release packaging changes.
-->

# Versioning Policy

## Scope

Plasmite ships one product line with multiple distribution surfaces:

- Rust crate + CLI (`Cargo.toml`)
- Python package (`bindings/python/pyproject.toml`)
- Node package (`bindings/node/package.json`)
- Node native crate (`bindings/node/native/Cargo.toml`)

## Compatibility Definition

- **CLI compatibility**: stable command behavior follows semantic versioning intent; breaking CLI behavior bumps major.
- **Bindings compatibility**: Python/Node API behavior tracks the same semantic version as CLI releases.
- **`libplasmite` ABI compatibility**: ABI changes are treated as release-significant and share the same version bump policy as CLI/bindings.

Until explicitly revised, compatibility is managed in one lockstep release train.

## Version Mapping (Lockstep)

For every release, the following versions must be identical:

- `Cargo.toml [package].version`
- `bindings/python/pyproject.toml [project].version`
- `bindings/node/package.json .version`
- `bindings/node/package-lock.json .version`
- `bindings/node/package-lock.json .packages[""].version`
- `bindings/node/native/Cargo.toml [package].version`

## Required Workflow

1. Run `scripts/bump_version.sh <version>` to apply one version everywhere.
2. Run `just check-version-alignment` (or `just ci-fast`) to verify no drift.
3. Commit all manifest updates together.

## Guardrails

- CI/local checks must fail when any mapped version drifts.
- Do not manually edit one manifest in isolation for release bumps.
- If policy changes away from lockstep, update this document and `scripts/check-version-alignment.sh` in the same change.
