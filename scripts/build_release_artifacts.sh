#!/usr/bin/env bash
# Purpose: Build the Rust artifacts consumed by every release package on one platform.
# Key inputs: target triple and optional --static flag.
# Role: Shared release build entrypoint for SDK, Python, Node, and packaging smoke.
# Invariants: One Cargo invocation emits binaries and every declared library crate type.
# Invariants: The cdylib has a stable platform identity.
# Invariants: --static requires the declared staticlib output used by SDK tarballs.

set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: build_release_artifacts.sh <target-triple> [--static]

examples:
  scripts/build_release_artifacts.sh aarch64-apple-darwin --static
  scripts/build_release_artifacts.sh x86_64-pc-windows-msvc
USAGE
}

if [[ $# -lt 1 || $# -gt 2 ]]; then
  usage
  exit 2
fi

target="$1"
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"$root_dir/scripts/release_target_field.sh" "$target" node_platform >/dev/null

build_static=false
if [[ $# -eq 2 ]]; then
  if [[ "$2" != "--static" ]]; then
    usage
    exit 2
  fi
  build_static=true
fi

cargo build --release --target "$target" --bins --lib

if [[ "$build_static" == true ]]; then
  case "$target" in
    *windows-msvc) static_lib="$root_dir/target/$target/release/plasmite.lib" ;;
    *) static_lib="$root_dir/target/$target/release/libplasmite.a" ;;
  esac
  if [[ ! -f "$static_lib" ]]; then
    echo "required static library was not produced: $static_lib" >&2
    exit 1
  fi
fi
