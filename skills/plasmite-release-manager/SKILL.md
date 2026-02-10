---
name: "plasmite-release-manager"
description: "Carefully run Plasmite releases end-to-end with fail-closed pre-release QA, split build/publish workflow mechanics, provenance validation, and post-release delivery verification across crates.io, npm, PyPI, Homebrew, and release artifacts. Use when asked to prepare, dry-run, execute, audit, or recover a release (including publish-only reruns after credential fixes)."
---

# Plasmite Release Manager

## Overview

Use this skill to run releases in a fail-closed way:
- run required QA gates before release
- stop on any failed or incomplete gate
- file blocker tasks in `ergo` under one release-blocker epic
- maintain one machine-readable evidence report through the full run
- execute split release mechanics with `gh` (`release` build, then `release-publish`)
- require publish preflight checks before any registry publish action
- support publish-only reruns from a successful build run ID after credential fixes
- verify build run provenance with `inspect_release_build_metadata.sh` before reruns
- verify that published packages are actually live

## Inputs

- `release_target`: version or tag being prepared (for example `v0.1.1`)
- `base_tag`: previous release tag used for regression comparisons
- `agent_id`: `model@host` for `ergo` claims/ownership
- `mode`: `dry-run` or `live`

Input contract (required):
- obtain all four inputs explicitly from the maintainer before execution
- do not infer `release_target`, `base_tag`, or `mode` from local tags/files unless the maintainer confirms
- if any input is missing or ambiguous, stop and ask before running gates
- initialize or reopen the evidence report:
  - `bash skills/plasmite-release-manager/scripts/init_release_evidence.sh --release-target <vX.Y.Z> --base-tag <vX.Y.Z> --mode <dry-run|live> --agent <model@host>`

## Execution Permissions (Required)

Request capable runtime access before starting release work:
- network access for GitHub and package registries (`gh`, crates.io, npm, PyPI, Homebrew checks)
- host auth/keychain access so `gh auth status` reflects the maintainer session
- ability to run repo QA/build commands without sandbox write restrictions

If commands fail due to sandbox/network, escalate immediately and re-run the same command.
Do not continue with partial/offline release evidence when a gate requires remote verification.

## Non-Negotiable Gate Policy

Any failed gate blocks release. Treat these as failures:
- explicit failing result
- critical tooling missing for the gate
- check not run / evidence incomplete

When blocked:
1. Stop release mechanics immediately (no tagging/publish).
2. File an `ergo` blocker task with `scripts/file_release_blocker.sh`.
3. Keep all blockers in one epic named `Release blockers: <release_target>`.

## Interruption Resume Protocol

If the run is interrupted (agent crash, user abort, runtime reset), do this before any new release action:
1. Re-open the evidence report in `.scratch/release/`.
2. Re-check current git/tag/workflow/blocker state using the checklist in `references/release-hygiene.md`.
3. Record resumed context (timestamp + agent + current checkpoint) in the evidence report.
4. Continue only from the first unchecked checkpoint; do not skip forward from memory.

## Workflow

1. Capture release context
   - Confirm explicit `release_target`, `base_tag`, and `mode` from maintainer input.
   - Verify `release_target` uses `vX.Y.Z` tag format and `base_tag` exists remotely.
   - Ensure `gh auth status` and `ergo where` are healthy.
   - Initialize/reopen evidence report with `scripts/init_release_evidence.sh`.
2. Run pre-release QA
   - Execute all required gates from `references/qa-gates.md`.
   - File blockers for every failed/incomplete gate.
3. Release only if zero blockers
   - Follow `references/release-hygiene.md`.
   - Use `gh` for split build/publish workflow handling and publish-only rerun dispatch when needed.
   - For publish-only reruns, validate `build_run_id` provenance before dispatch.
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

When a GitHub run failed, prefer the evidence wrapper:

```bash
skills/plasmite-release-manager/scripts/file_release_blocker_with_evidence.sh \
  --release-target "v0.1.1" \
  --check "Binding parity & packaging health" \
  --title "Fix runner tooling mismatch for release smoke scripts" \
  --summary "release workflow failed in packaging smoke stage." \
  --run-id "12345678901" \
  --failing-command "bash scripts/node_pack_smoke.sh" \
  --agent "codex@$(hostname -s)"
```

The wrapper enriches blocker summaries with run URL, failed job names, and optional log snippets.

## Bundled Resources

- `references/qa-gates.md`
  - gate-by-gate commands, evidence, and blocker criteria
- `references/release-hygiene.md`
  - mechanical release steps with `gh`
- `references/delivery-verification.md`
  - verify packages are live post-release
- `scripts/file_release_blocker.sh`
  - deterministic blocker filing into `ergo`
- `scripts/file_release_blocker_with_evidence.sh`
  - blocker filing with attached run metadata and log excerpt
- `scripts/init_release_evidence.sh`
  - creates/reopens the release evidence artifact used for resumes and handoffs
- `scripts/check_release_tooling_contract.sh`
  - enforces CI tooling compatibility for release scripts/workflow before tagging
- `scripts/inspect_release_build_metadata.sh`
  - validates release build run provenance and prints metadata for safe publish-only reruns
