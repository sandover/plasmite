#!/usr/bin/env bash
# Stage Cargo's shared library under its ABI-major filename.
set -euo pipefail
directory="$1"
case "${2:-$(uname -s)}" in
  *apple-darwin|Darwin) source=libplasmite.dylib; versioned=libplasmite.2.dylib ;;
  *windows*|MINGW*|MSYS*) source=plasmite.dll; versioned=plasmite2.dll ;;
  *linux*|Linux) source=libplasmite.so; versioned=libplasmite.so.2 ;;
  *) echo "Unsupported shared-library platform: ${2:-$(uname -s)}" >&2; exit 1 ;;
esac
if [[ "$versioned" == *.dll ]]; then
  cp "$directory/$source" "$directory/$versioned"
else
  ln -sf "$source" "$directory/$versioned"
fi
