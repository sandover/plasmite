#!/usr/bin/env bash
# Purpose: Fail fast when release versions drift across published ecosystems.
# Key outputs: Compares Cargo, Python, Node, and Node lock/native versions.
# Role: CI/local guardrail for lockstep release policy.
# Invariants: All listed manifests must carry one identical semantic version.
# Notes: Prints a small mismatch report to speed troubleshooting.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

extract_toml_section_version() {
  local file="$1"
  local section="$2"
  awk -v section="$section" '
    $0 == "[" section "]" { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && /^version = "/ {
      line = $0
      sub(/^version = "/, "", line)
      sub(/".*$/, "", line)
      print line
      exit
    }
  ' "$file"
}

cargo_version="$(extract_toml_section_version "$repo_root/Cargo.toml" "package")"
python_version="$(extract_toml_section_version "$repo_root/bindings/python/pyproject.toml" "project")"
node_native_version="$(extract_toml_section_version "$repo_root/bindings/node/native/Cargo.toml" "package")"
node_version="$(jq -r '.version' "$repo_root/bindings/node/package.json")"
node_lock_version="$(jq -r '.version' "$repo_root/bindings/node/package-lock.json")"
node_lock_root_version="$(jq -r '.packages[""].version' "$repo_root/bindings/node/package-lock.json")"

if [[ -z "$cargo_version" || -z "$python_version" || -z "$node_version" || -z "$node_native_version" || -z "$node_lock_version" || -z "$node_lock_root_version" ]]; then
  echo "error: could not parse one or more version fields" >&2
  exit 1
fi

if [[ "$cargo_version" != "$python_version" || "$cargo_version" != "$node_version" || "$cargo_version" != "$node_native_version" || "$cargo_version" != "$node_lock_version" || "$cargo_version" != "$node_lock_root_version" ]]; then
  echo "error: version alignment check failed (lockstep policy)." >&2
  echo "  Cargo.toml:                    $cargo_version" >&2
  echo "  bindings/python/pyproject.toml: $python_version" >&2
  echo "  bindings/node/package.json:     $node_version" >&2
  echo "  bindings/node/package-lock.json: $node_lock_version" >&2
  echo "  bindings/node/package-lock.json packages[\"\"].version: $node_lock_root_version" >&2
  echo "  bindings/node/native/Cargo.toml: $node_native_version" >&2
  echo "hint: run scripts/bump_version.sh <version> to update all files together." >&2
  exit 1
fi

echo "version alignment ok: $cargo_version"
