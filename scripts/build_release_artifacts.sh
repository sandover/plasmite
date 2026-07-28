#!/usr/bin/env bash
# Purpose: Build the Rust artifacts consumed by every release package on one platform.
# Key inputs: target triple and optional --static flag.
# Role: Shared release build entrypoint for SDK, Python, Node, and packaging smoke.
# Invariants: Always emits plasmite/pls and a cdylib with a stable platform identity.
# Invariants: --static additionally emits libplasmite.a for SDK tarballs.

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

cargo build --release --target "$target" --bins

if [[ "$target" == *windows-msvc ]]; then
  cargo rustc --release --target "$target" --lib --crate-type=cdylib
elif [[ "$target" == *apple-darwin ]]; then
  cargo rustc --release --target "$target" --lib --crate-type=cdylib -- \
    -C link-arg=-Wl,-install_name,@rpath/libplasmite.dylib
else
  cargo rustc --release --target "$target" --lib --crate-type=cdylib -- \
    -C link-arg=-Wl,-soname,libplasmite.so
fi

if [[ "$build_static" == true ]]; then
  cargo rustc --release --target "$target" --lib --crate-type=staticlib
fi
