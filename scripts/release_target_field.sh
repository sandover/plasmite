#!/usr/bin/env bash
# Purpose: Read one field for a Rust target from the canonical release manifest.
# Role: Shared lookup for packaging scripts.

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: release_target_field.sh <rust-target> <field>" >&2
  exit 2
fi

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target="$1"
field="$2"
manifest="$root_dir/release/targets.json"

case "$field" in
  sdk_platform|node_platform|build_sdk) ;;
  *)
    echo "error: unsupported release target field: $field" >&2
    exit 2
    ;;
esac

value="$(jq -r --arg target "$target" --arg field "$field" '
  [.targets[] | select(.rust_target == $target)][0][$field] // empty
' "$manifest")"
if [[ -z "$value" ]]; then
  echo "error: release target or field not found: $target $field" >&2
  exit 1
fi
printf '%s\n' "$value"
