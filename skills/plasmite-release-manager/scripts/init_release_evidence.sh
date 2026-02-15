#!/usr/bin/env bash
# Purpose: Initialize or reopen a single release evidence artifact for one release target.
# Key outputs: Markdown report path under .scratch/release and stable checkpoint sections.
# Role: Make release runs resumable and auditable across interruptions and handoffs.
# Invariants: One evidence file per release target; never silently overwrite without --force.
# Invariants: Captures release context (target/base/mode/agent) before gate execution.
# Notes: Safe to run repeatedly; existing file is reused unless --force is provided.

set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  init_release_evidence.sh \
    --release-target <vX.Y.Z> \
    --mode <dry-run|live> \
    --agent <model@host> \
    [--base-tag <vX.Y.Z>] \
    [--force]
USAGE
}

release_target=""
base_tag=""
mode=""
agent_id=""
force=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release-target)
      release_target="${2:-}"
      shift 2
      ;;
    --base-tag)
      base_tag="${2:-}"
      shift 2
      ;;
    --mode)
      mode="${2:-}"
      shift 2
      ;;
    --agent)
      agent_id="${2:-}"
      shift 2
      ;;
    --force)
      force=1
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

if [[ -z "$release_target" || -z "$mode" || -z "$agent_id" ]]; then
  echo "error: missing required args" >&2
  usage
  exit 2
fi

if [[ ! "$release_target" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: release target must match vX.Y.Z" >&2
  exit 2
fi

if [[ "$mode" != "dry-run" && "$mode" != "live" ]]; then
  echo "error: mode must be dry-run or live" >&2
  exit 2
fi

derive_base_tag() {
  local target="$1"
  local tags
  local prev=""
  tags="$(git tag --list 'v[0-9]*.[0-9]*.[0-9]*' | sort -V || true)"
  while IFS= read -r tag; do
    [[ -z "$tag" ]] && continue
    # Stop once we reach target or any newer/equal version.
    if [[ "$(printf '%s\n%s\n' "$tag" "$target" | sort -V | head -n1)" == "$target" ]]; then
      break
    fi
    prev="$tag"
  done <<< "$tags"
  if [[ -n "$prev" ]]; then
    printf '%s\n' "$prev"
    return 0
  fi
  echo "error: could not derive base tag below '$target'; pass --base-tag explicitly." >&2
  return 1
}

if [[ -z "$base_tag" ]]; then
  base_tag="$(derive_base_tag "$release_target")"
fi

if [[ ! "$base_tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: base tag must match vX.Y.Z" >&2
  exit 2
fi

mkdir -p .scratch/release
version="${release_target#v}"
out_path=".scratch/release/evidence-v${version}.md"

if [[ -f "$out_path" && "$force" -ne 1 ]]; then
  echo "$out_path"
  exit 0
fi

timestamp="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

cat > "$out_path" <<EOF_REPORT
# Release Evidence Report: ${release_target}

## Release Context
- Release target: ${release_target}
- Base tag: ${base_tag}
- Mode: ${mode}
- Agent: ${agent_id}
- Initialized (UTC): ${timestamp}

## Resume Checkpoint
- Last updated (UTC): ${timestamp}
- Current phase: context
- Local branch/SHA: _pending_
- Remote tag state: _pending_
- Last workflow run ID: _pending_
- Last workflow run URL: _pending_
- Blocker epic ID: _pending_
- Open blocker count: _pending_

## QA Gate Evidence
- Gate 0 Release workflow topology:
- Gate 1 Dependency/vulnerability:
- Gate 2 Memory safety:
- Gate 3 Concurrency/crash consistency:
- Gate 5 Performance:
- Gate 6 API/CLI stability:
- Gate 7 Documentation alignment:
- Gate 8 Binding parity/packaging:
- Gate 9 Server/UI security:
- Gate 11 Licensing/notices:

## Release Mechanics
- Version alignment:
- Tag creation:
- Tag push:
- Workflow progression:

## Delivery Verification
- GitHub release artifacts:
- crates.io:
- npm:
- PyPI:
- Homebrew:
- Binding install sanity:

## Blockers and Decisions
- _none_
EOF_REPORT

echo "$out_path"
