#!/usr/bin/env bash
# Purpose: List manifest targets participating in one delivery channel.
# Role: Shared target selection for delivery and documentation checks.

set -euo pipefail

if [[ $# -lt 1 || $# -gt 3 ]]; then
  echo "usage: release_channel_targets.sh <channel> [tier] [field]" >&2
  exit 2
fi

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
channel="$1"
tier="${2:-official}"
field="${3:-sdk_platform}"
manifest="$root_dir/release/targets.json"

case "$channel" in
  github_sdk|homebrew|npm|pypi|cargo_binstall) ;;
  *) echo "error: unknown release channel: $channel" >&2; exit 2 ;;
esac
case "$field" in
  rust_target|sdk_platform|node_platform) ;;
  *) echo "error: unsupported target field: $field" >&2; exit 2 ;;
esac
case "$tier" in
  official|preview) ;;
  *) echo "error: tier must be official or preview" >&2; exit 2 ;;
esac

jq -r --arg channel "$channel" --arg tier "$tier" --arg field "$field" '
  .targets[]
  | select(.channels[$channel] == $tier)
  | .[$field]
' "$manifest"
