#!/usr/bin/env bash
# Purpose: Validate the canonical release target manifest.
# Role: Fail closed before release automation consumes target metadata.
# Invariants: Target and artifact identifiers are unique and channel combinations are supported.

set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="${1:-$root_dir/release/targets.json}"

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required to validate release targets" >&2
  exit 2
fi
if [[ ! -f "$manifest" ]]; then
  echo "error: release target manifest not found: $manifest" >&2
  exit 1
fi

jq -e '
  def tier: . == null or . == "official" or . == "preview";
  def unique_by_field($field):
    (map(.[$field]) | length) == (map(.[$field]) | unique | length);

  .schema_version == 1 and
  (.targets | type == "array" and length > 0) and
  (.targets | all(
    (.rust_target | type == "string" and length > 0) and
    (.runner | type == "string" and length > 0) and
    (.sdk_platform | type == "string" and length > 0) and
    (.node_platform | type == "string" and length > 0) and
    (.build_sdk | type == "boolean") and
    (.upload_sdist | type == "boolean") and
    (.upload_wheel | type == "boolean") and
    (.channels | keys | sort) ==
      ["cargo_binstall", "github_sdk", "homebrew", "npm", "pypi"] and
    (.channels | all(tier)) and
    (if .build_sdk then .channels.github_sdk != null else .channels.github_sdk == null end) and
    (if .channels.homebrew != null then
       .build_sdk and
       (.sdk_platform == "darwin_amd64" or
        .sdk_platform == "darwin_arm64" or
        .sdk_platform == "linux_amd64")
     else true end) and
    (if .channels.cargo_binstall != null then .build_sdk else true end)
    and (if .channels.npm != null then .node_platform != "linux-arm64" and .node_platform != "linux-armv7" else true end)
    and (if .channels.pypi != null then .upload_wheel else true end)
    and (if .channels.github_sdk == "preview" and (.rust_target == "aarch64-unknown-linux-gnu" or .rust_target == "armv7-unknown-linux-gnueabihf") then
      .channels.homebrew == null and .channels.npm == null and .channels.pypi == null and .channels.cargo_binstall == null and
      (.upload_sdist | not) and (.upload_wheel | not)
    else true end)
    and (if .rust_target == "aarch64-unknown-linux-gnu" then
      .runner == "ubuntu-22.04-arm" and .sdk_platform == "linux_arm64" and .channels.github_sdk == "preview"
    elif .rust_target == "armv7-unknown-linux-gnueabihf" then
      .runner == "ubuntu-22.04" and .sdk_platform == "linux_armv7" and .channels.github_sdk == "preview"
    else true end)
  )) and
  (.targets | unique_by_field("rust_target")) and
  (.targets | unique_by_field("sdk_platform")) and
  (.targets | unique_by_field("node_platform"))
' "$manifest" >/dev/null || {
  echo "error: invalid release target manifest: $manifest" >&2
  exit 1
}

echo "release target manifest ok: $manifest"
