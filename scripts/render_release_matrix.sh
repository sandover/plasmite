#!/usr/bin/env bash
# Purpose: Render the GitHub Actions build matrix from release/targets.json.
# Role: Keep workflow target identity derived from the canonical manifest.

set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="${1:-$root_dir/release/targets.json}"

"$root_dir/scripts/validate_release_targets.sh" "$manifest" >/dev/null
jq -c '[
  .targets[] | {
    os: .runner,
    target: .rust_target,
    sdk_platform,
    node_platform,
    sdk: .build_sdk,
    upload_sdist,
    upload_wheel
  }
]' "$manifest"
