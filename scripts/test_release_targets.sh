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

test_manifest="$tmp_dir/non-publishing-target.json"
jq '.targets += [{
  rust_target: "aarch64-unknown-linux-gnu",
  runner: "ubuntu-24.04-arm",
  sdk_platform: "linux_arm64",
  node_platform: "linux-arm64",
  build_sdk: true,
  upload_sdist: false,
  upload_wheel: false,
  channels: {
    github_sdk: "preview",
    homebrew: null,
    npm: null,
    pypi: null,
    cargo_binstall: null
  }
}]' "$manifest" >"$test_manifest"

matrix="$("$root_dir/scripts/render_release_matrix.sh" "$test_manifest")"
test_row="$(jq -c '.[] | select(.target == "aarch64-unknown-linux-gnu")' <<<"$matrix")"
[[ "$(jq -r '.os' <<<"$test_row")" == "ubuntu-24.04-arm" ]]
[[ "$(jq -r '.sdk_platform' <<<"$test_row")" == "linux_arm64" ]]
[[ "$(jq -r '"plasmite_9.9.9_\(.sdk_platform).tar.gz"' <<<"$test_row")" == \
  "plasmite_9.9.9_linux_arm64.tar.gz" ]]

echo "release target validator fixtures ok"
