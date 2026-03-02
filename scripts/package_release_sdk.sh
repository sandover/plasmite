#!/usr/bin/env bash
# Purpose: Assemble a release tarball that follows the published SDK layout.
# Exports: dist/plasmite_<version>_<platform>.tar.gz
# Role: Single packaging entrypoint for GitHub release workflow.
# Invariants: Tarball root always contains bin/, include/, and lib/pkgconfig/.
# Invariants: Shared lib identity is normalized for relocatable use where possible.
# Notes: Writes temporary staging data under .scratch/ and keeps workspace clean.

set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <target-triple> <platform-tag> <version>" >&2
  exit 1
fi

target="$1"
platform="$2"
version="$3"

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
release_dir="$root_dir/target/$target/release"
mkdir -p "$root_dir/.scratch"

if [[ ! -d "$release_dir" ]]; then
  echo "release directory not found: $release_dir" >&2
  exit 1
fi

stage_dir="$(mktemp -d "$root_dir/.scratch/release-sdk.XXXXXX")"
trap 'rm -rf "$stage_dir"' EXIT
sdk_dir="$stage_dir/sdk"

mkdir -p "$sdk_dir/bin" "$sdk_dir/include" "$sdk_dir/lib/pkgconfig" "$root_dir/dist"

cp "$release_dir/plasmite" "$sdk_dir/bin/plasmite"
cp "$release_dir/pls" "$sdk_dir/bin/pls"
cp "$root_dir/include/plasmite.h" "$sdk_dir/include/plasmite.h"

if [[ -f "$release_dir/libplasmite.dylib" ]]; then
  cp "$release_dir/libplasmite.dylib" "$sdk_dir/lib/libplasmite.dylib"
  # Keep dylib identity relocatable for Homebrew and bundled package consumers.
  install_name_tool -id "@rpath/libplasmite.dylib" "$sdk_dir/lib/libplasmite.dylib"
elif [[ -f "$release_dir/libplasmite.so" ]]; then
  cp "$release_dir/libplasmite.so" "$sdk_dir/lib/libplasmite.so"
else
  echo "shared lib not found in $release_dir (expected libplasmite.dylib or libplasmite.so)" >&2
  exit 1
fi

if [[ -f "$release_dir/libplasmite.a" ]]; then
  cp "$release_dir/libplasmite.a" "$sdk_dir/lib/libplasmite.a"
fi

libs_private=""
if [[ "$target" == *linux* ]]; then
  # Keep static consumers working with `pkg-config --static --libs plasmite`.
  libs_private="Libs.private: -lpthread -ldl -lm"
fi

{
cat <<EOF
prefix=\${pcfiledir}/../..
exec_prefix=\${prefix}
libdir=\${exec_prefix}/lib
includedir=\${prefix}/include

Name: plasmite
Description: Plasmite C ABI library
Version: ${version}
Libs: -L\${libdir} -lplasmite
EOF
if [[ -n "$libs_private" ]]; then
  echo "$libs_private"
fi
cat <<EOF
Cflags: -I\${includedir}
EOF
} > "$sdk_dir/lib/pkgconfig/plasmite.pc"

tar -C "$sdk_dir" -czf "$root_dir/dist/plasmite_${version}_${platform}.tar.gz" .
