#!/usr/bin/env bash
# Purpose: Update all lockstep version fields from one explicit input version.
# Key outputs: Rewrites crate, Python, Node, Node lockfile, and native crate versions.
# Role: Maintainer tool to reduce release-time manual edits and drift.
# Invariants: [package]/[project] version keys stay valid and synchronized.
# Notes: Runs alignment check at the end and exits non-zero on parse/update errors.

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: scripts/bump_version.sh <version>" >&2
  exit 2
fi

new_version="$1"
if [[ ! "$new_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "error: version must look like semver (e.g. 0.2.0 or 0.2.0-rc.1)" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

update_toml_section_version() {
  local file="$1"
  local section="$2"
  local tmp
  tmp="$(mktemp)"
  awk -v section="$section" -v version="$new_version" '
    BEGIN { in_section = 0; updated = 0 }
    $0 == "[" section "]" { in_section = 1; print; next }
    /^\[/ { in_section = 0 }
    in_section && /^version = "/ && !updated {
      print "version = \"" version "\""
      updated = 1
      next
    }
    { print }
    END {
      if (!updated) {
        exit 3
      }
    }
  ' "$file" > "$tmp"
  mv "$tmp" "$file"
}

update_toml_section_version "$repo_root/Cargo.toml" "package"
update_toml_section_version "$repo_root/bindings/python/pyproject.toml" "project"
update_toml_section_version "$repo_root/bindings/node/native/Cargo.toml" "package"

tmp_json="$(mktemp)"
jq --arg version "$new_version" '.version = $version' \
  "$repo_root/bindings/node/package.json" > "$tmp_json"
mv "$tmp_json" "$repo_root/bindings/node/package.json"

tmp_lock="$(mktemp)"
jq --arg version "$new_version" \
  '.version = $version | .packages[""].version = $version' \
  "$repo_root/bindings/node/package-lock.json" > "$tmp_lock"
mv "$tmp_lock" "$repo_root/bindings/node/package-lock.json"

"$repo_root/scripts/check-version-alignment.sh"
echo "bumped versions to $new_version"
