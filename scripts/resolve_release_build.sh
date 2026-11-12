#!/usr/bin/env bash
# Purpose: Select a release build and reject inconsistent provenance before publishing.
# Inputs: repository, optional run ID, optional tag, rehearsal boolean.
# Output: build_run_id, tag, version, source_sha and rehearsal as key=value lines.
# Uses gh/git for current facts; never publishes or modifies a checkout.
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: resolve_release_build.sh <owner/repository> <build-run-id-or-empty> <tag-or-empty> <true|false>" >&2
  exit 2
fi
GITHUB_REPOSITORY="$1"
requested_build_run_id="$2"
requested_release_tag="$3"
rehearsal="$4"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

if [[ "$rehearsal" != "true" && "$rehearsal" != "false" ]]; then
  echo "error: rehearsal must be true or false." >&2
  exit 1
fi
if [[ -n "$requested_build_run_id" && ! "$requested_build_run_id" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: build_run_id must contain only decimal digits and be greater than zero." >&2
  exit 1
fi
if [[ -n "$requested_release_tag" ]]; then
  "$script_dir/validate_release_tag.sh" "$requested_release_tag"
fi

resolve_build_run_by_tag() {
  local expected_tag="$1"
  local search_dir="$2"
  local run_id candidate_dir metadata_file candidate_tag

  local successful_runs
  successful_runs="$(gh run list \
    --repo "$GITHUB_REPOSITORY" \
    --workflow release.yml \
    --limit 40 \
    --json databaseId,conclusion \
    --jq '.[] | select(.conclusion=="success") | .databaseId')" || return 1

  while IFS= read -r run_id; do
    [[ -n "$run_id" ]] || continue
    [[ "$run_id" =~ ^[1-9][0-9]*$ ]] || return 1
    candidate_dir="${search_dir}/${run_id}"
    mkdir -p "$candidate_dir"
    if ! gh run download "$run_id" --repo "$GITHUB_REPOSITORY" --name release-metadata --dir "$candidate_dir" >/dev/null 2>&1; then
      continue
    fi
    metadata_file="$(find "$candidate_dir" -name metadata.json | head -n 1)"
    if [[ -z "$metadata_file" ]]; then
      continue
    fi
    candidate_tag="$(jq -r '.tag // ""' "$metadata_file")" || return 1
    if [[ "$candidate_tag" == "$expected_tag" ]]; then
      echo "$run_id"
      return 0
    fi
  done <<< "$successful_runs"

  return 1
}

if [[ -n "$requested_build_run_id" && -n "$requested_release_tag" ]]; then
  echo "error: provide only one of build_run_id or release_tag." >&2
  exit 1
fi
if [[ -z "$requested_build_run_id" && -z "$requested_release_tag" ]]; then
  echo "error: provide build_run_id or release_tag." >&2
  exit 1
fi

if [[ -n "$requested_release_tag" ]]; then
  if ! build_run_id="$(resolve_build_run_by_tag "$requested_release_tag" "$tmp_dir/tag-lookup")"; then
    echo "error: no successful release workflow run found for tag '$requested_release_tag'." >&2
    echo "hint: run release.yml for that tag, or pass build_run_id explicitly." >&2
    exit 1
  fi
else
  build_run_id="$requested_build_run_id"
  echo "warning: build_run_id path is for incident recovery; prefer release_tag for routine publishes." >&2
  echo "::warning::build_run_id path is for incident recovery; prefer release_tag for routine publishes." >&2
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    echo "⚠️ Using build_run_id incident-recovery path. Prefer release_tag for routine publishes." >> "$GITHUB_STEP_SUMMARY"
  fi
fi

if [[ ! "$build_run_id" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: resolved build run ID is not a positive decimal integer." >&2
  exit 1
fi

run_json="$(gh run view "$build_run_id" --repo "$GITHUB_REPOSITORY" --json workflowName,conclusion,url)"
workflow_name="$(jq -r '.workflowName' <<<"$run_json")"
run_conclusion="$(jq -r '.conclusion' <<<"$run_json")"
if [[ "$workflow_name" != "release" ]]; then
  echo "error: build_run_id=$build_run_id belongs to workflow '$workflow_name', expected 'release'." >&2
  exit 1
fi
if [[ "$run_conclusion" != "success" ]]; then
  echo "error: build_run_id=$build_run_id is not successful (conclusion=$run_conclusion)." >&2
  exit 1
fi

# Extract tag/version from build run metadata.
selected_run_dir="$tmp_dir/selected-run"
mkdir -p "$selected_run_dir"
gh run download "$build_run_id" --repo "$GITHUB_REPOSITORY" --name release-metadata --dir "$selected_run_dir"
metadata_file="$(find "$selected_run_dir" -name metadata.json -print -quit)"
if [[ -z "$metadata_file" ]]; then
  echo "error: release-metadata artifact missing metadata.json for run $build_run_id." >&2
  exit 1
fi

tag="$(jq -er '.tag | select(type == "string")' "$metadata_file")"
version="$(jq -er '.version | select(type == "string")' "$metadata_file")"
source_sha="$(jq -er '.source_sha | select(type == "string")' "$metadata_file")"
source_ref="$(jq -er '.source_ref | select(type == "string")' "$metadata_file")"
source_run_id="$(jq -er '.source_run_id | select(type == "string")' "$metadata_file")"
source_workflow="$(jq -er '.source_workflow | select(type == "string")' "$metadata_file")"
"$script_dir/validate_release_tag.sh" "$tag"
if [[ -n "$requested_release_tag" && "$tag" != "$requested_release_tag" ]]; then
  echo "error: selected release metadata tag '$tag' does not match requested tag '$requested_release_tag'." >&2
  exit 1
fi
expected_version="${tag#v}"
if [[ "$version" != "$expected_version" ]]; then
  echo "error: release metadata version '$version' does not match tag '$tag'." >&2
  exit 1
fi
if [[ ! "$source_sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: invalid source_sha in release metadata: '$source_sha'." >&2
  exit 1
fi
if [[ "$source_ref" != "refs/tags/${tag}" ]]; then
  echo "error: release metadata source_ref '$source_ref' does not match tag '$tag'." >&2
  exit 1
fi
if [[ "$source_run_id" != "$build_run_id" || "$source_workflow" != "release" ]]; then
  echo "error: release metadata does not belong to build run $build_run_id." >&2
  exit 1
fi

tag_source_sha="$("$script_dir/resolve_release_tag_sha.sh" "$GITHUB_REPOSITORY" "$tag")"
if [[ "$source_sha" != "$tag_source_sha" ]]; then
  echo "error: release build source $source_sha does not match current tag target $tag_source_sha for '$tag'." >&2
  exit 1
fi

{
  echo "build_run_id=$build_run_id"
  echo "tag=$tag"
  echo "version=$version"
  echo "source_sha=$source_sha"
  echo "rehearsal=$rehearsal"
}
