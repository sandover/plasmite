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
expect_invalid unsupported-homebrew '(.targets[] | select(.rust_target == "x86_64-pc-windows-msvc").channels.homebrew) = "official"'
expect_invalid armv7-npm '(.targets[] | select(.rust_target == "armv7-unknown-linux-gnueabihf").channels.npm) = "preview"'
expect_invalid arm64-wheel '(.targets[] | select(.rust_target == "aarch64-unknown-linux-gnu").upload_wheel) = true'
expect_invalid newer-arm64-runner '(.targets[] | select(.rust_target == "aarch64-unknown-linux-gnu").runner) = "ubuntu-24.04-arm"'

matrix="$("$root_dir/scripts/render_release_matrix.sh" "$manifest")"
[[ "$(jq -r '. | length' <<<"$matrix")" == 6 ]]
for target in aarch64-unknown-linux-gnu armv7-unknown-linux-gnueabihf; do
  row="$(jq -c --arg target "$target" '.[] | select(.target == $target)' <<<"$matrix")"
  [[ -n "$row" ]]
  [[ "$(jq -r '.sdk and (.build_node | not) and (.build_python | not) and (.upload_sdist | not) and (.upload_wheel | not)' <<<"$row")" == true ]]
done
[[ "$(jq -r '.[] | select(.target == "aarch64-unknown-linux-gnu") | .os' <<<"$matrix")" == ubuntu-22.04-arm ]]
[[ "$(jq -r '.[] | select(.target == "armv7-unknown-linux-gnueabihf") | .armv7_cross' <<<"$matrix")" == true ]]
[[ "$(jq -r '.[] | select(.target == "x86_64-unknown-linux-gnu") | .build_python and .upload_sdist and .build_node' <<<"$matrix")" == true ]]
[[ "$(jq -r '.[] | select(.target == "x86_64-pc-windows-msvc") | .build_python and .upload_wheel and .build_node' <<<"$matrix")" == true ]]
preview_sdks="$("$root_dir/scripts/release_channel_targets.sh" github_sdk preview sdk_platform | sort)"
[[ "$preview_sdks" == $'linux_arm64\nlinux_armv7' ]]

echo "release target validator fixtures ok"
