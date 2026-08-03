#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "Lite3 sanitizer tests require Linux so Clang sanitizer runtimes can be linked." >&2
  exit 2
fi

command -v clang >/dev/null || {
  echo "Lite3 sanitizer tests require clang." >&2
  exit 2
}

echo "Lite3 sanitizer environment:"
rustc -Vv
clang --version

export CC=clang
export CFLAGS="-fsanitize=address,undefined -fno-omit-frame-pointer"
export RUSTFLAGS="-C linker=clang -C link-arg=-fsanitize=address,undefined"
export ASAN_OPTIONS="detect_leaks=1:halt_on_error=1"
export UBSAN_OPTIONS="halt_on_error=1:print_stacktrace=1"

cargo test core::lite3::tests --lib

echo "Public C ABI sanitizer harness:"
cargo build --lib
./scripts/abi_adversarial.sh
