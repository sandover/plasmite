#!/usr/bin/env bash
# Purpose: Provide shared SDK path normalization for release smoke/package scripts.
# Key exports: plasmite_normalize_sdk_dir <input-dir> <staging-dir> [context-label].
# Role: Convert raw cargo output layouts into stable SDK bin/lib layout when needed.
# Invariants: Returned directory always contains bin/plasmite + lib/libplasmite.(dylib|so).
# Invariants: Normalized SDK inputs are returned unchanged and are never copied.
# Invariants: Staged outputs only include required runtime artifacts and optional static library.
# Notes: Intended to be sourced by scripts; emits actionable errors to stderr.

plasmite_normalize_sdk_dir() {
  if [[ $# -lt 2 || $# -gt 3 ]]; then
    echo "usage: plasmite_normalize_sdk_dir <input-dir> <staging-dir> [context-label]" >&2
    return 2
  fi

  local input_dir="$1"
  local staging_dir="$2"
  local context_label="${3:-SDK path}"

  if [[ ! -d "$input_dir" ]]; then
    echo "error: ${context_label} directory not found: $input_dir" >&2
    return 1
  fi

  # Already normalized SDK layout.
  if [[ -d "$input_dir/bin" && -d "$input_dir/lib" ]]; then
    if [[ ! -f "$input_dir/bin/plasmite" ]]; then
      echo "error: ${context_label} missing CLI binary: $input_dir/bin/plasmite" >&2
      return 1
    fi
    if [[ ! -f "$input_dir/lib/libplasmite.dylib" && ! -f "$input_dir/lib/libplasmite.so" ]]; then
      echo "error: ${context_label} missing shared library under: $input_dir/lib" >&2
      return 1
    fi
    echo "$input_dir"
    return 0
  fi

  # Treat input as raw cargo output layout (for example target/release).
  if [[ ! -f "$input_dir/plasmite" ]]; then
    echo "error: ${context_label} missing raw cargo binary: $input_dir/plasmite" >&2
    return 1
  fi
  if [[ ! -f "$input_dir/libplasmite.dylib" && ! -f "$input_dir/libplasmite.so" ]]; then
    echo "error: ${context_label} missing raw cargo shared library in: $input_dir" >&2
    return 1
  fi

  rm -rf "$staging_dir"
  mkdir -p "$staging_dir/bin" "$staging_dir/lib"

  cp "$input_dir/plasmite" "$staging_dir/bin/plasmite"
  chmod +x "$staging_dir/bin/plasmite"

  if [[ -f "$input_dir/libplasmite.dylib" ]]; then
    cp "$input_dir/libplasmite.dylib" "$staging_dir/lib/libplasmite.dylib"
  else
    cp "$input_dir/libplasmite.so" "$staging_dir/lib/libplasmite.so"
  fi

  if [[ -f "$input_dir/libplasmite.a" ]]; then
    cp "$input_dir/libplasmite.a" "$staging_dir/lib/libplasmite.a"
  fi

  if [[ -f "$input_dir/pls" ]]; then
    cp "$input_dir/pls" "$staging_dir/bin/pls"
    chmod +x "$staging_dir/bin/pls"
  fi

  echo "$staging_dir"
}
