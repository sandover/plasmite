#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lock_file="$repo_root/vendor/lite3.lock.json"
repository="$(jq -er '.repository' "$lock_file")"
pinned_commit="$(jq -er '.commit' "$lock_file")"
upstream_commit="$(git ls-remote "$repository" refs/heads/main | awk '{print $1}')"

[[ "$upstream_commit" =~ ^[0-9a-f]{40}$ ]] || {
  echo "failed to resolve Lite3 upstream main" >&2
  exit 1
}

if [[ "$upstream_commit" != "$pinned_commit" ]]; then
  repository_web="${repository%.git}"
  echo "Lite3 has upstream commits after the pinned snapshot." >&2
  echo "Pinned:   $pinned_commit" >&2
  echo "Upstream: $upstream_commit" >&2
  echo "Review:   $repository_web/compare/$pinned_commit...$upstream_commit" >&2
  echo "Update:   just update-lite3 $upstream_commit" >&2
  exit 1
fi

echo "Lite3 pin matches upstream main at $pinned_commit"
