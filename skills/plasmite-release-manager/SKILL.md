---
name: "plasmite-release-manager"
description: "Carefully run Plasmite releases end-to-end with fail-closed pre-release QA, GitHub release mechanics, and post-release delivery verification across crates.io, npm, PyPI, Homebrew, and release artifacts. Use when asked to prepare, dry-run, execute, or audit a release."
---

# Plasmite Release Manager

## Overview

Use this skill to run releases in a fail-closed way:
- run required QA gates before release
- stop on any failed or incomplete gate
- file blocker tasks in `ergo` under one release-blocker epic
- execute release mechanics with `gh`
- verify that published packages are actually live

## Inputs

- `release_target`: version or tag being prepared (for example `v0.1.1`)
- `base_tag`: previous release tag used for regression comparisons
- `agent_id`: `model@host` for `ergo` claims/ownership
- `mode`: `dry-run` or `live`

## Non-Negotiable Gate Policy

Any failed gate blocks release. Treat these as failures:
- explicit failing result
- critical tooling missing for the gate
- check not run / evidence incomplete

When blocked:
1. Stop release mechanics immediately (no tagging/publish).
2. File an `ergo` blocker task with `scripts/file_release_blocker.sh`.
3. Keep all blockers in one epic named `Release blockers: <release_target>`.

## Workflow

1. Capture release context
   - Determine `release_target`, `base_tag`, and `mode`.
   - Ensure `gh auth status` and `ergo where` are healthy.
2. Run pre-release QA
   - Execute all required gates from `references/qa-gates.md`.
   - File blockers for every failed/incomplete gate.
3. Release only if zero blockers
   - Follow `references/release-hygiene.md`.
   - Use `gh` for tag/release workflow handling.
4. Verify delivery
   - Run checks from `references/delivery-verification.md`.
   - File blocker tasks for missing artifacts or version mismatches.

## Required QA Gates

Run all of these before release:
1. Dependency & vulnerability monitoring
2. Memory safety & unsafe boundaries
3. Concurrency correctness & crash consistency
5. Performance regression guard
6. API/CLI stability & compatibility
7. Documentation alignment (docs match reality)
8. Binding parity & packaging health
9. Server / web UI security review
11. Licensing & notices

Detailed commands, stop conditions, and evidence are in `references/qa-gates.md`.

## Blocker Filing

Use the helper script for every failed gate:

```bash
skills/plasmite-release-manager/scripts/file_release_blocker.sh \
  --release-target "v0.1.1" \
  --check "Performance regression guard" \
  --title "Investigate benchmark regression in get(seq)" \
  --summary "Bench run is 18% slower than base tag v0.1.0 on same host." \
  --agent "codex@$(hostname -s)"
```

The script will:
- create/find epic `Release blockers: <release_target>`
- create a task with required sections (goal/background/acceptance/gates/consult)
- print created epic/task IDs

## Bundled Resources

- `references/qa-gates.md`
  - gate-by-gate commands, evidence, and blocker criteria
- `references/release-hygiene.md`
  - mechanical release steps with `gh`
- `references/delivery-verification.md`
  - verify packages are live post-release
- `scripts/file_release_blocker.sh`
  - deterministic blocker filing into `ergo`
