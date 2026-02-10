# Release Process Simplification Proposal

Status: Proposal (future consideration, no immediate workflow changes)  
Date: 2026-02-10  
Owner: Maintainers

## Context

The recent `0.1.9` release consumed roughly 1.5 days across multiple Codex sessions.
That duration is not acceptable for normal patch releases.

This proposal records simplification options that reduce release operator load while
preserving critical distribution guarantees.

## Problem Statement

Release friction came less from one catastrophic defect and more from cumulative
operational overhead:

- too many mandatory checks regardless of change scope
- long recovery/resume instructions after interruptions
- duplicated logic between skill guidance and CI workflows
- hard-to-assess, low-signal checks mixed with high-signal safety gates

## Goals

- reduce active maintainer effort and decision fatigue
- preserve predictable, fail-closed release behavior
- keep Homebrew, crates.io, npm, PyPI, and GitHub release artifacts aligned
- avoid adding new automation surface area during stabilization

## Non-Goals

- no immediate rewrite of release workflows
- no change to published channel set
- no loosening of provenance or Homebrew alignment requirements

## Invariants To Keep

These remain release-blocking:

1. Publish provenance from a successful `release` build run (`build_run_id` + metadata).
2. Homebrew formula alignment before registry publish.
3. Fail-closed publish flow (no silent partial success).
4. One explicit local performance gate per candidate when performance-sensitive code changed.

## Candidate Simplifications

### 1) Convert tooling-contract policing into checklist + rehearsal trigger

Current pain:
- static regex-heavy checks can drift and create maintenance burden.

Proposal:
- replace mandatory tooling-contract gate with a checklist rule:
  - if release workflow/scripts changed since last release, require one successful
    `release-publish` rehearsal before live publish.

Expected benefit:
- lower false positives and less static-contract churn.

Risk:
- subtle topology regressions may be detected later.

Guardrail:
- rehearsal remains mandatory after topology changes.

### 2) Move from universal QA gates to risk-scoped QA profiles

Current pain:
- all gates are run for all releases, even when diff scope is small.

Proposal:
- define two profiles in skill docs:
  - always-run core profile
  - conditional profile triggered by path/diff heuristics

Always-run core profile:
- version alignment
- fmt/clippy/tests
- release build success
- publish preflight
- Homebrew alignment
- delivery verification

Conditional profile examples:
- performance gate when core/storage/hot path changes
- security deep review when server/auth/network surface changes
- dependency deep audit when lockfiles/deps change materially

Expected benefit:
- major reduction in unnecessary gate runtime.

Risk:
- misclassification could skip a useful check.

Guardrail:
- maintainers can force full profile anytime.

### 3) Replace long interruption protocol with short checkpoint resume

Current pain:
- resume instructions are too large for normal interruptions.

Proposal:
- default resume checklist (short):
  - `git status --short --branch`
  - latest `release` run ID + conclusion
  - latest `release-publish` run ID + conclusion
  - whether Homebrew tap commit is already pushed

Use current long protocol only for incident mode:
- partial publish
- provenance mismatch
- conflicting tags/runs

Expected benefit:
- faster, less error-prone recovery in normal pauses.

Risk:
- operators may miss latent state drift.

Guardrail:
- escalate to full incident checklist on first anomaly.

### 4) Simplify blocker process for release execution

Current pain:
- required blocker filing for every incomplete check adds process overhead.

Proposal:
- during live release execution, require blocker filing only for:
  - release-blocking workflow failures
  - channel mismatch incidents
  - policy exceptions requiring follow-up

Expected benefit:
- less bookkeeping during high-pressure release windows.

Risk:
- reduced paper trail for minor issues.

Guardrail:
- maintain summary notes in release evidence; post-release cleanup can file
  follow-up tasks as needed.

### 5) Trim delivery verification to mandatory and periodic checks

Current pain:
- exhaustive post-release install checks are expensive every time.

Proposal:
- mandatory every release:
  - registry version checks
  - GitHub release artifact presence
  - Homebrew formula alignment verification
- periodic or incident-triggered:
  - full clean-environment install sanity across all bindings

Expected benefit:
- shorter tail phase while preserving distribution confidence.

Risk:
- delayed discovery of install UX regressions.

Guardrail:
- run full install sanity on cadence (for example weekly) and on dependency/toolchain shifts.

## Proposed Adoption Plan (No Immediate Code Changes)

Phase 0 (now):
- keep current workflows unchanged
- update skill/release docs to label mandatory vs conditional checks

Phase 1 (after 1-2 stable releases):
- remove or downgrade low-signal checks only after confirming no regression in release reliability

Phase 2:
- consider workflow simplification (for example removing partial-release switches) only if
  policy remains strict no-partial-release for two consecutive releases

## Success Criteria

Adopt simplifications only if all are true for at least two releases:

1. No increase in failed live publish attempts.
2. No channel lag incidents (especially Homebrew).
3. Active human release time trends toward < 90 minutes for patch releases.
4. No provenance/alignment invariant violations.

## Open Questions

1. Do maintainers want to permanently disallow partial-release controls in CI?
2. What diff/path policy should trigger mandatory performance gate runs?
3. Should full install sanity become scheduled CI instead of per-release manual verification?

## Decision Record Linkage

If accepted, promote this document to `docs/record/` and update:

- `docs/record/releasing.md`
- `skills/plasmite-release-manager/SKILL.md`
- `skills/plasmite-release-manager/references/qa-gates.md`
- `skills/plasmite-release-manager/references/release-hygiene.md`
