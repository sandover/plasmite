#!/usr/bin/env bash
# Purpose: File a release blocker task with workflow/run evidence attached.
# Key outputs: A blocker task in the release-blocker epic with enriched summary details.
# Role: Reduce repetitive manual triage work during failed release workflow handling.
# Invariants: Delegates task creation to file_release_blocker.sh to keep epic/task schema stable.
# Invariants: Includes run metadata when provided (run id/url, failed jobs, failing command).
# Notes: Accepts optional log snippet file and appends a short excerpt to the blocker summary.

set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  file_release_blocker_with_evidence.sh \
    --release-target <tag-or-version> \
    --check <gate-name> \
    --title <task-title> \
    --summary <short-summary> \
    [--run-id <github-run-id>] \
    [--run-url <url>] \
    [--failing-command <command>] \
    [--log-snippet-file <path>] \
    [--agent <model@host>] \
    [--dry-run]
USAGE
}

release_target=""
check_name=""
task_title=""
summary=""
run_id=""
run_url=""
failing_command=""
log_snippet_file=""
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
    --run-id)
      run_id="${2:-}"
      shift 2
      ;;
    --run-url)
      run_url="${2:-}"
      shift 2
      ;;
    --failing-command)
      failing_command="${2:-}"
      shift 2
      ;;
    --log-snippet-file)
      log_snippet_file="${2:-}"
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

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
base_script="${script_dir}/file_release_blocker.sh"

if [[ ! -x "$base_script" ]]; then
  echo "error: missing executable helper: ${base_script}" >&2
  exit 3
fi

failed_jobs=""
if [[ -n "$run_id" ]]; then
  if ! command -v gh >/dev/null 2>&1; then
    echo "error: gh CLI required when --run-id is set" >&2
    exit 3
  fi
  if ! command -v jq >/dev/null 2>&1; then
    echo "error: jq CLI required when --run-id is set" >&2
    exit 3
  fi

  run_json="$(gh run view "$run_id" --json url,jobs 2>/dev/null || true)"
  if [[ -n "$run_json" ]]; then
    if [[ -z "$run_url" ]]; then
      run_url="$(printf '%s\n' "$run_json" | jq -r '.url // empty')"
    fi
    failed_jobs="$(printf '%s\n' "$run_json" | jq -r '[.jobs[]? | select(.conclusion=="failure") | .name] | join(", ")')"
  fi
fi

summary_enriched="$summary"

if [[ -n "$run_id" ]]; then
  summary_enriched+=" run_id=${run_id}."
fi
if [[ -n "$run_url" ]]; then
  summary_enriched+=" run_url=${run_url}."
fi
if [[ -n "$failed_jobs" ]]; then
  summary_enriched+=" failed_jobs=${failed_jobs}."
fi
if [[ -n "$failing_command" ]]; then
  summary_enriched+=" failing_command=${failing_command}."
fi
if [[ -n "$log_snippet_file" && -f "$log_snippet_file" ]]; then
  excerpt="$(sed -n '1,12p' "$log_snippet_file" | tr '\n' ' ' | tr -s ' ' | cut -c1-360)"
  if [[ -n "$excerpt" ]]; then
    summary_enriched+=" log_excerpt=${excerpt}."
  fi
fi

args=(
  --release-target "$release_target"
  --check "$check_name"
  --title "$task_title"
  --summary "$summary_enriched"
)

if [[ -n "$agent_id" ]]; then
  args+=(--agent "$agent_id")
fi
if [[ "$dry_run" -eq 1 ]]; then
  args+=(--dry-run)
fi

"$base_script" "${args[@]}"
