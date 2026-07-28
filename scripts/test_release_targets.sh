#!/usr/bin/env bash
# Purpose: Exercise fail-closed release target manifest validation.
# Role: Deterministic fixture test for malformed and unsupported target data.

set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root_dir/release/targets.json"
validator="$root_dir/scripts/validate_release_targets.sh"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

"$validator" "$manifest" >/dev/null

expect_invalid() {
  local name="$1"
  local filter="$2"
  local fixture="$tmp_dir/$name.json"
  jq "$filter" "$manifest" >"$fixture"
  if "$validator" "$fixture" >/dev/null 2>&1; then
    echo "error: validator accepted invalid fixture: $name" >&2
    exit 1
  fi
}

expect_invalid duplicate-target '.targets += [.targets[0]]'
expect_invalid missing-runner 'del(.targets[0].runner)'
expect_invalid unsupported-homebrew '.targets[3].channels.homebrew = "official"'

echo "release target validator fixtures ok"
