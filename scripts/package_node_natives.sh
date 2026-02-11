#!/usr/bin/env bash
# Purpose: Stage Node native artifacts into native/{platform}/ for npm packaging.
# Key outputs: Populated bindings/node/native/<platform> directory.
# Role: Shared local/CI helper for multi-platform Node package assembly.
# Invariants: Copies index.node, libplasmite.{so|dylib}, and plasmite CLI binary.
# Invariants: Fails fast when required source artifacts are missing.
# Notes: Accepts explicit SDK and optional addon path to keep inputs explicit.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODE_DIR="$ROOT/bindings/node"

usage() {
  cat <<'USAGE'
Usage:
  scripts/package_node_natives.sh <platform> <sdk_dir> [addon_path]

Examples:
  scripts/package_node_natives.sh linux-x64 target/release bindings/node/index.node
  scripts/package_node_natives.sh darwin-arm64 target/aarch64-apple-darwin/release
USAGE
}

if [[ "$#" -lt 2 || "$#" -gt 3 ]]; then
  usage
  exit 2
fi

platform="$1"
sdk_dir="$2"
addon_path="${3:-$NODE_DIR/index.node}"

if [[ ! -d "$sdk_dir" ]]; then
  echo "error: sdk_dir does not exist: $sdk_dir" >&2
  exit 1
fi

if [[ ! -f "$addon_path" ]]; then
  echo "error: addon_path does not exist: $addon_path" >&2
  exit 1
fi

if [[ "$platform" == darwin-* ]]; then
  lib_name="libplasmite.dylib"
else
  lib_name="libplasmite.so"
fi

lib_candidates=(
  "$sdk_dir/lib/$lib_name"
  "$sdk_dir/$lib_name"
)
cli_candidates=(
  "$sdk_dir/bin/plasmite"
  "$sdk_dir/plasmite"
)

find_first_existing() {
  local candidate
  for candidate in "$@"; do
    if [[ -f "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

lib_source="$(find_first_existing "${lib_candidates[@]}")" || {
  echo "error: $lib_name not found under sdk_dir=$sdk_dir" >&2
  exit 1
}

cli_source="$(find_first_existing "${cli_candidates[@]}")" || {
  echo "error: plasmite CLI not found under sdk_dir=$sdk_dir" >&2
  exit 1
}

dest_dir="$NODE_DIR/native/$platform"
rm -rf "$dest_dir"
mkdir -p "$dest_dir"
cp "$addon_path" "$dest_dir/index.node"
cp "$lib_source" "$dest_dir/$lib_name"
cp "$cli_source" "$dest_dir/plasmite"
chmod +x "$dest_dir/plasmite"

echo "ok: packaged node native artifacts into $dest_dir"
