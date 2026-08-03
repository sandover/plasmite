#!/usr/bin/env bash
# Purpose: Compile and run focused supported-misuse checks against the public C ABI.
# Invariants: Arbitrary invalid addresses, stale handles, and double-free remain caller UB.

set -euo pipefail

root_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
build_dir=${1:-"$root_dir/target/debug"}
compiler=${CC:-cc}

if [[ ! -d "$build_dir" ]]; then
  echo "build directory not found: $build_dir" >&2
  exit 1
fi

mkdir -p "$root_dir/.scratch"
work_dir=$(mktemp -d "$root_dir/.scratch/abi-adversarial.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT
mkdir -p "$work_dir/pools"

compile_flags=()
if [[ -n "${CFLAGS:-}" ]]; then
  read -r -a compile_flags <<< "$CFLAGS"
fi

"$compiler" "${compile_flags[@]}" \
  -I "$root_dir/include" \
  "$root_dir/tests/abi_adversarial.c" \
  -L "$build_dir" -lplasmite \
  -o "$work_dir/abi_adversarial"

if [[ "$(uname)" == "Darwin" ]]; then
  DYLD_LIBRARY_PATH="$build_dir" "$work_dir/abi_adversarial" "$work_dir/pools"
else
  LD_LIBRARY_PATH="$build_dir" "$work_dir/abi_adversarial" "$work_dir/pools"
fi
