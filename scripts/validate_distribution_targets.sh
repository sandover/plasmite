#!/usr/bin/env bash
# Purpose: Validate human-authored distribution target lists against the manifest.
# Role: Prevent support-tier prose from drifting from release automation.

set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root_dir/release/targets.json"
document="$root_dir/docs/record/distribution.md"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

jq -r '
  .targets[]
  | select([.channels[]] | any(. == "official"))
  | .rust_target
' "$manifest" | sort -u >"$tmp_dir/manifest-official"

awk '
  /^Official platforms:/ { capture=1; next }
  /^Not currently targeted:/ { capture=0 }
  capture { print }
' "$document" |
  grep -o '`[^`]*`' |
  tr -d '`' |
  grep -E '^[A-Za-z0-9_]+-[A-Za-z0-9_]+-[A-Za-z0-9_-]+$' |
  sort -u >"$tmp_dir/docs-official"

"$root_dir/scripts/release_channel_targets.sh" cargo_binstall preview rust_target |
  sort -u >"$tmp_dir/manifest-binstall"
sed -n '/cargo-binstall.*`preview`/,/release-publish smoke gate/p' "$document" |
  grep -o '`[^`]*`' |
  tr -d '`' |
  grep -E '^[A-Za-z0-9_]+-[A-Za-z0-9_]+-[A-Za-z0-9_-]+$' |
  sort -u >"$tmp_dir/docs-binstall"

if ! diff -u "$tmp_dir/manifest-official" "$tmp_dir/docs-official"; then
  echo "error: official platform list contradicts release/targets.json" >&2
  exit 1
fi
if ! diff -u "$tmp_dir/manifest-binstall" "$tmp_dir/docs-binstall"; then
  echo "error: cargo-binstall preview list contradicts release/targets.json" >&2
  exit 1
fi

echo "distribution target documentation ok"
