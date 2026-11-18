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
case "$target" in
  x86_64-apple-darwin) export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-10.12}" ;;
  aarch64-apple-darwin) export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}" ;;
esac
if [[ $# -eq 2 ]]; then
  if [[ "$2" != "--static" ]]; then
    usage
    exit 2
  fi
  build_static=true
fi

# Git Bash has its own link.exe. Put the configured Visual C++ linker first.
if [[ "$target" == *windows-msvc && -n "${VCToolsInstallDir:-}" ]]; then
  msvc_dir="$(cygpath -u "$VCToolsInstallDir")"
  export PATH="$msvc_dir/bin/Hostx64/x64:$PATH"
fi
cargo build --release --locked --target "$target" --bins --lib
bash "$root_dir/scripts/stage_versioned_library.sh" "$root_dir/target/$target/release" "$target"

if [[ "$target" == *windows-msvc ]]; then
  (
    cd "$root_dir/target/$target/release"
    # Cargo's import library names plasmite.dll. Generate one that names the
    # distributed DLL and includes exactly the exports present in that build.
    exports="$(dumpbin.exe -nologo -exports plasmite2.dll | awk '/^[[:space:]]*[0-9]+[[:space:]]+[[:xdigit:]]+[[:space:]]+[[:xdigit:]]+[[:space:]]+plsm_/ { print $4 }')"
    [[ -n "$exports" ]] || { echo "no Plasmite exports found in plasmite2.dll" >&2; exit 1; }
    trap 'rm -f plasmite2.def plasmite2.exp' EXIT
    printf 'LIBRARY plasmite2.dll\nEXPORTS\n%s\n' "$exports" > plasmite2.def
    lib.exe -nologo -machine:x64 -def:plasmite2.def -out:plasmite2.lib
  )
fi

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
