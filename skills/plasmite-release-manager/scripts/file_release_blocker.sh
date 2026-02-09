#!/usr/bin/env bash
# Purpose: Create/find a release-blocker epic and file blocker tasks in ergo.
# Key outputs: Epic ID and task ID for each release-blocking finding.
# Role: Deterministic release QA failure intake for pre-release and post-release checks.
# Invariants: Every failed gate maps to one actionable task with required sections.
# Invariants: Blocker tasks are grouped under "Release blockers: <release_target>".
# Notes: Supports --dry-run for validation without mutating ergo state.

set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  file_release_blocker.sh \
    --release-target <tag-or-version> \
    --check <gate-name> \
    --title <task-title> \
    --summary <short-summary> \
    [--agent <model@host>] \
    [--dry-run]
USAGE
}

release_target=""
check_name=""
task_title=""
summary=""
agent_id=""
dry_run=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release-target)
      release_target="${2:-}"
      shift 2
      ;;
    --check)
      check_name="${2:-}"
      shift 2
      ;;
    --title)
      task_title="${2:-}"
      shift 2
      ;;
    --summary)
      summary="${2:-}"
      shift 2
      ;;
    --agent)
      agent_id="${2:-}"
      shift 2
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown arg: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [[ -z "$release_target" || -z "$check_name" || -z "$task_title" || -z "$summary" ]]; then
  echo "error: missing required args" >&2
  usage
  exit 2
fi

if ! command -v ergo >/dev/null 2>&1; then
  echo "error: ergo CLI not found in PATH" >&2
  exit 3
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq not found in PATH" >&2
  exit 3
fi

epic_title="Release blockers: ${release_target}"

epics_json="$(ergo --json list --epics)"
epic_id="$(printf '%s\n' "$epics_json" | jq -r --arg t "$epic_title" '.[] | select(.title==$t) | .id' | head -n 1)"

epic_body=$(cat <<EOF
## Goal
- Track release-blocking findings for ${release_target} and prevent release until resolved.

## Background / Rationale
- Release QA is fail-closed in this repo.
- Any failed or incomplete release gate must become tracked work.

## Acceptance Criteria
- Every blocker for ${release_target} is represented by a task in this epic.
- Release proceeds only when blocker tasks are resolved or explicitly canceled with rationale.

## Automated Validation Gates
- \`ergo --json list --epic <this-epic-id>\`
- \`ergo --json list --ready --epic <this-epic-id>\`

## Consult Me
- If a blocker implies release-scope changes (target matrix, packaging policy, or breaking API policy), stop and request maintainer decision before proceeding.
EOF
)

if [[ -z "$epic_id" ]]; then
  if [[ "$dry_run" -eq 1 ]]; then
    echo "dry-run: would create epic: ${epic_title}"
    epic_id="DRYRUN_EPIC"
  else
    new_epic_payload="$(jq -nc --arg title "$epic_title" --arg body "$epic_body" '{title:$title, body:$body}')"
    new_epic_resp="$(printf '%s' "$new_epic_payload" | ergo --json new epic)"
    epic_id="$(printf '%s\n' "$new_epic_resp" | jq -r '.id')"
    echo "created epic: ${epic_id} (${epic_title})"
  fi
else
  echo "using existing epic: ${epic_id} (${epic_title})"
fi

task_body=$(cat <<EOF
## Goal
- Resolve release blocker: ${check_name}

## Background / Rationale
- Release target: ${release_target}
- Gate that failed or was incomplete: ${check_name}
- Summary: ${summary}

## Acceptance Criteria
- Root cause is identified and fixed (or explicitly accepted with maintainer approval).
- The ${check_name} gate is re-run and passes with evidence.
- Release notes/docs are updated if behavior or policy changed.

## Automated Validation Gates
- Re-run gate: ${check_name}
- \`just ci-fast\`

## Consult Me
- If this fix changes public CLI/API/binding behavior or release scope, stop and request maintainer decision before continuing.
EOF
)

full_task_title="[release-blocker][${release_target}] ${task_title}"

if [[ "$dry_run" -eq 1 ]]; then
  echo "dry-run: would create task under epic ${epic_id}: ${full_task_title}"
  exit 0
fi

new_task_payload="$(jq -nc \
  --arg title "$full_task_title" \
  --arg body "$task_body" \
  --arg epic "$epic_id" \
  '{title:$title, body:$body, epic:$epic}')"

new_task_resp="$(printf '%s' "$new_task_payload" | ergo --json new task)"
task_id="$(printf '%s\n' "$new_task_resp" | jq -r '.id')"

echo "created task: ${task_id}"
echo "epic: ${epic_id}"
if [[ -n "$agent_id" ]]; then
  echo "note: agent hint provided (${agent_id}); task is unclaimed by default."
fi
