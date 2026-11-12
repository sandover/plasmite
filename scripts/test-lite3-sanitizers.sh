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
export RUSTFLAGS="-C linker=clang -C link-arg=-fsanitize=address,undefined"
export ASAN_OPTIONS="detect_leaks=1:halt_on_error=1"
export UBSAN_OPTIONS="halt_on_error=1:print_stacktrace=1"

# Optimization can erase a check made after __builtin_assume_aligned.
# Keep both compiler paths in the regular CI job.
for optimization in 0 2; do
  echo "Lite3 sanitizers with C optimization -O$optimization:"
  export CFLAGS="-O$optimization -fsanitize=address,undefined -fno-omit-frame-pointer"
  cargo test core::lite3::tests --lib
  cargo test abi_errors_report_invalid_lite3 --lib

  echo "Public C ABI sanitizer harness:"
  cargo build --lib
  ./scripts/abi_adversarial.sh
done
