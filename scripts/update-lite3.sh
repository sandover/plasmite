#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lock_file="$repo_root/vendor/lite3.lock.json"
manifest_file="$repo_root/vendor/lite3.sha256"
vendor_dir="$repo_root/vendor/lite3"
upstream_url="https://github.com/fastserial/lite3.git"

usage() {
  echo "usage: $0 <40-character upstream commit>" >&2
  exit 2
}

[[ $# -eq 1 ]] || usage
requested_commit="$1"
[[ "$requested_commit" =~ ^[0-9a-fA-F]{40}$ ]] || usage

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/plasmite-lite3.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT

git clone --quiet --no-checkout "$upstream_url" "$work_dir/upstream"
git -C "$work_dir/upstream" checkout --quiet --detach "$requested_commit"

resolved_commit="$(git -C "$work_dir/upstream" rev-parse HEAD)"
normalized_commit="$(printf '%s' "$requested_commit" | tr '[:upper:]' '[:lower:]')"
if [[ "$resolved_commit" != "$normalized_commit" ]]; then
  echo "requested Lite3 commit did not resolve exactly: $requested_commit" >&2
  exit 1
fi

snapshot_dir="$work_dir/snapshot"
mkdir -p "$snapshot_dir"

tracked_files=(
  LICENSE
  README.md
  include/lite3.h
  include/lite3_context_api.h
  lib/nibble_base64/LICENSE
  lib/nibble_base64/base64.c
  lib/nibble_base64/base64.h
  lib/yyjson/LICENSE
  lib/yyjson/yyjson.c
  lib/yyjson/yyjson.h
  src/ctx_api.c
  src/debug.c
  src/json_dec.c
  src/json_enc.c
  src/lite3.c
)

for path in "${tracked_files[@]}"; do
  source_path="$work_dir/upstream/$path"
  [[ -f "$source_path" ]] || {
    echo "required Lite3 source is missing at commit $resolved_commit: $path" >&2
    exit 1
  }
  mkdir -p "$snapshot_dir/$(dirname "$path")"
  cp "$source_path" "$snapshot_dir/$path"
done

mkdir -p "$vendor_dir"
rsync -a --delete "$snapshot_dir/" "$vendor_dir/"

pin_date="$(git -C "$work_dir/upstream" show -s --format=%cs "$resolved_commit")"
printf '{\n  "repository": "%s",\n  "commit": "%s",\n  "commit_date": "%s"\n}\n' \
  "$upstream_url" "$resolved_commit" "$pin_date" >"$lock_file"

(
  cd "$repo_root"
  find vendor/lite3 -type f | LC_ALL=C sort | xargs shasum -a 256
) >"$manifest_file"

"$repo_root/scripts/verify-lite3.sh"
echo "updated Lite3 snapshot to $resolved_commit"
