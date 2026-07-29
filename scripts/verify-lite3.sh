#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lock_file="$repo_root/vendor/lite3.lock.json"
manifest_file="$repo_root/vendor/lite3.sha256"

[[ -f "$lock_file" ]] || {
  echo "missing Lite3 lock file: vendor/lite3.lock.json" >&2
  exit 1
}
[[ -f "$manifest_file" ]] || {
  echo "missing Lite3 integrity manifest: vendor/lite3.sha256" >&2
  exit 1
}

repository="$(jq -er '.repository' "$lock_file")"
commit="$(jq -er '.commit' "$lock_file")"
commit_date="$(jq -er '.commit_date' "$lock_file")"

[[ "$repository" == "https://github.com/fastserial/lite3.git" ]] || {
  echo "unexpected Lite3 repository in vendor/lite3.lock.json" >&2
  exit 1
}
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || {
  echo "Lite3 lock must contain a full lowercase commit SHA" >&2
  exit 1
}
[[ "$commit_date" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || {
  echo "Lite3 lock must contain an ISO commit date" >&2
  exit 1
}

(
  cd "$repo_root"
  shasum -a 256 --check vendor/lite3.sha256
)

expected_files="$(
  sed -E 's/^[0-9a-f]{64}  //' "$manifest_file" | LC_ALL=C sort
)"
actual_files="$(
  cd "$repo_root"
  find vendor/lite3 -type f | LC_ALL=C sort
)"
if [[ "$actual_files" != "$expected_files" ]]; then
  echo "vendor/lite3 contains files not represented by vendor/lite3.sha256" >&2
  diff -u <(printf '%s\n' "$expected_files") <(printf '%s\n' "$actual_files") || true
  exit 1
fi

echo "Lite3 snapshot verified at $commit ($commit_date)"
