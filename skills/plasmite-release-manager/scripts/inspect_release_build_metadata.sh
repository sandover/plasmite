#!/usr/bin/env bash
# Purpose: Validate and summarize metadata from a completed release build workflow run.
# Key exports: Prints run + metadata summary as JSON for operator review.
# Role: Guard publish-only reruns by proving build_run_id provenance before dispatch.
# Invariants: Run must belong to workflow "release" and have conclusion "success".
# Invariants: Embedded release metadata must include a v-tag and matching semantic version.
# Invariants: Optional expected tag check fails closed on mismatch.
# Notes: Uses gh + jq and downloads only the release-metadata artifact for the run.

set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: inspect_release_build_metadata.sh --run-id <id> [--expect-tag <vX.Y.Z>]
USAGE
}

run_id=""
expect_tag=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --run-id)
      run_id="${2:-}"
      shift 2
      ;;
    --expect-tag)
      expect_tag="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [[ -z "$run_id" ]]; then
  echo "error: --run-id is required" >&2
  usage
  exit 2
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "error: gh CLI is required" >&2
  exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required" >&2
  exit 2
fi

run_json="$(gh run view "$run_id" --json workflowName,conclusion,event,url,headSha,headBranch)"
workflow_name="$(jq -r '.workflowName' <<<"$run_json")"
run_conclusion="$(jq -r '.conclusion' <<<"$run_json")"

if [[ "$workflow_name" != "release" ]]; then
  echo "error: run $run_id belongs to workflow '$workflow_name' (expected 'release')." >&2
  exit 1
fi
if [[ "$run_conclusion" != "success" ]]; then
  echo "error: run $run_id is not successful (conclusion=$run_conclusion)." >&2
  exit 1
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

gh run download "$run_id" --name release-metadata --dir "$tmp_dir"
metadata_file="$(find "$tmp_dir" -name metadata.json | head -n 1)"
if [[ -z "$metadata_file" ]]; then
  echo "error: release-metadata artifact missing metadata.json for run $run_id." >&2
  exit 1
fi

meta_tag="$(jq -r '.tag' "$metadata_file")"
meta_version="$(jq -r '.version' "$metadata_file")"
meta_source_sha="$(jq -r '.source_sha' "$metadata_file")"
meta_source_ref="$(jq -r '.source_ref' "$metadata_file")"

if [[ -z "$meta_tag" || "$meta_tag" == "null" || "$meta_tag" != v* ]]; then
  echo "error: release metadata tag is invalid: '$meta_tag'." >&2
  exit 1
fi
if [[ "$meta_version" != "${meta_tag#v}" ]]; then
  echo "error: release metadata version '$meta_version' does not match tag '$meta_tag'." >&2
  exit 1
fi
if [[ -n "$expect_tag" && "$meta_tag" != "$expect_tag" ]]; then
  echo "error: run metadata tag '$meta_tag' does not match expected tag '$expect_tag'." >&2
  exit 1
fi

jq -n \
  --arg run_id "$run_id" \
  --arg workflow_name "$workflow_name" \
  --arg run_conclusion "$run_conclusion" \
  --arg run_url "$(jq -r '.url' <<<"$run_json")" \
  --arg run_event "$(jq -r '.event' <<<"$run_json")" \
  --arg run_head_sha "$(jq -r '.headSha' <<<"$run_json")" \
  --arg run_head_branch "$(jq -r '.headBranch' <<<"$run_json")" \
  --arg metadata_tag "$meta_tag" \
  --arg metadata_version "$meta_version" \
  --arg metadata_source_sha "$meta_source_sha" \
  --arg metadata_source_ref "$meta_source_ref" \
  '{run_id:$run_id,workflow_name:$workflow_name,run_conclusion:$run_conclusion,run_url:$run_url,run_event:$run_event,run_head_sha:$run_head_sha,run_head_branch:$run_head_branch,metadata_tag:$metadata_tag,metadata_version:$metadata_version,metadata_source_sha:$metadata_source_sha,metadata_source_ref:$metadata_source_ref}'
