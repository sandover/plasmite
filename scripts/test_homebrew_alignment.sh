#!/usr/bin/env bash
# Purpose: Verify manifest-driven Homebrew update and alignment behavior.
# Role: Local fixture test with no network or tap mutation.

set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
tap_dir="$tmp_dir/tap"
formula="$tap_dir/Formula/plasmite.rb"
old_sums="$tmp_dir/old-sums.txt"
new_sums="$tmp_dir/new-sums.txt"
mkdir -p "$(dirname "$formula")"

{
  echo 'class Plasmite < Formula'
  echo '  version "0.0.1"'
  while IFS= read -r platform; do
    [[ -n "$platform" ]] || continue
    echo "  url \"https://github.com/sandover/plasmite/releases/download/v0.0.1/plasmite_0.0.1_${platform}.tar.gz\""
    echo "  sha256 \"old-${platform}\""
    echo "old-${platform}  plasmite_0.0.1_${platform}.tar.gz" >>"$old_sums"
    echo "new-${platform}  plasmite_9.9.9_${platform}.tar.gz" >>"$new_sums"
  done < <("$root_dir/scripts/release_channel_targets.sh" homebrew official sdk_platform)
  echo 'end'
} >"$formula"

"$root_dir/scripts/verify_homebrew_formula_alignment.sh" \
  --version 0.0.1 \
  --sha256sums "$old_sums" \
  --formula-file "$formula" >/dev/null
"$root_dir/scripts/update_homebrew_formula.sh" \
  v9.9.9 \
  "$tap_dir" \
  --sha256sums "$new_sums" >/dev/null
"$root_dir/scripts/verify_homebrew_formula_alignment.sh" \
  --version 9.9.9 \
  --sha256sums "$new_sums" \
  --formula-file "$formula" >/dev/null

echo "manifest-driven Homebrew fixtures ok"
