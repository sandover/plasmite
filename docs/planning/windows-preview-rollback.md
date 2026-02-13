# Windows preview rollback policy (CI gate)

## Goal
Define how the Windows smoke gate behaves during preview so we get early signal without destabilizing `main`.

## Preview mode (initial)
- Gate mode: **non-blocking**.
- Implementation rule: Windows smoke jobs run with `continue-on-error: true` while preview is active.
- Scope: applies to the fast Windows smoke gate in `.github/workflows/ci.yml`.

## Promotion criteria (preview → blocking)
Promote to blocking only after all of the following are true:
- `10` consecutive green `main` runs for the Windows smoke gate.
- No unresolved Windows smoke incidents.
- Maintainer sign-off by `@brandonharvey`.

## Rollback trigger
Trigger rollback when either condition occurs:
- `2` or more Windows smoke failures in the latest `5` `main` runs, or
- one confirmed deterministic regression that reproduces on rerun.

## Rollback action
When rollback is triggered:
1. Keep/restore non-blocking mode (`continue-on-error: true`).
2. If failures are noisy and non-actionable, temporarily disable the PR path filter trigger for Windows smoke until triage lands.
3. Open/refresh a rollback tracking issue with run links, failure signature, and owner.
4. Re-enable stricter mode only after the trigger condition clears and owner signs off.

## Owner and escalation
- Primary owner: `@brandonharvey`.
- Backup owner: current release owner for the active release cycle.
- Escalation SLA: acknowledge within one business day and publish chosen rollback action in the tracking issue.

## CI mapping
This policy is surfaced in `.github/workflows/ci.yml` via preview policy env/config values and a summary job so each CI run shows active mode, thresholds, and owners.
