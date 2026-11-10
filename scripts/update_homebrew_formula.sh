#!/usr/bin/env bash
# Purpose: Update homebrew-tap Formula/plasmite.rb to match a specific release.
# Key exports: Rewrites version, release URLs, and sha256 values for supported targets.
# Role: Keep Homebrew distribution aligned with every Plasmite release.
# Invariants: Requires checksums for every Homebrew target in release/targets.json.
# Notes: Checksums can come from an existing GitHub release, a release build run ID, or a local sums file.

set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: update_homebrew_formula.sh <version-tag> <homebrew-tap-path> [--build-run-id <id> | --sha256sums <path>] [--prepare-only]

Normal updates require verified public SDK downloads. --prepare-only renders a
temporary candidate for rehearsal; never push that candidate before publication.

examples:
  scripts/update_homebrew_formula.sh v0.1.9 ../homebrew-tap
  scripts/update_homebrew_formula.sh v0.1.9 ../homebrew-tap --build-run-id 12345678901
  scripts/update_homebrew_formula.sh v0.1.9 ../homebrew-tap --sha256sums .scratch/release/sha256sums.txt
USAGE
}

if [[ $# -eq 1 && ( "$1" == "-h" || "$1" == "--help" ) ]]; then
  usage
  exit 0
fi

if [[ $# -lt 2 ]]; then
  usage
  exit 2
fi

version_tag="$1"
tap_path="$2"
shift 2

build_run_id=""
sha256_file=""
prepare_only=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prepare-only)
      prepare_only=true
      shift
      ;;
    --build-run-id)
      build_run_id="${2:-}"
      shift 2
      ;;
    --sha256sums)
      sha256_file="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [[ -n "$build_run_id" && -n "$sha256_file" ]]; then
  echo "error: pass only one checksum source: --build-run-id or --sha256sums" >&2
  exit 2
fi

if [[ "$version_tag" != v* ]]; then
  echo "error: version tag must look like vX.Y.Z (got '$version_tag')" >&2
  exit 2
fi

version="${version_tag#v}"
formula_file="${tap_path}/Formula/plasmite.rb"
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ ! -f "$formula_file" ]]; then
  echo "error: formula file not found at $formula_file" >&2
  exit 1
fi

if [[ "$prepare_only" == true && -e "$tap_path/.git" ]]; then
  echo 'error: --prepare-only requires a temporary candidate, not a tap Git checkout.' >&2
  exit 1
fi

tmp_dir="$(mktemp -d)"
candidate=""
trap 'rm -rf "$tmp_dir"; [[ -z "$candidate" ]] || rm -f "$candidate" "$candidate.bak"' EXIT
verification_sha_file=""

load_sha_from_file() {
  local file="$1"
  if [[ ! -f "$file" ]]; then
    echo "error: sha256 file not found: $file" >&2
    exit 1
  fi

  local platform
  local sha
  while IFS= read -r platform; do
    [[ -n "$platform" ]] || continue
    sha="$(grep "plasmite_${version}_${platform}.tar.gz" "$file" | awk '{print $1}' | head -n1)"
    if [[ -z "$sha" ]]; then
      echo "error: missing checksum for ${platform} in $file" >&2
      exit 1
    fi
  done < <("$root_dir/scripts/release_channel_targets.sh" homebrew official sdk_platform)
}

if [[ -n "$sha256_file" ]]; then
  load_sha_from_file "$sha256_file"
  verification_sha_file="$sha256_file"
elif [[ -n "$build_run_id" ]]; then
  if ! command -v gh >/dev/null 2>&1; then
    echo "error: gh CLI is required for --build-run-id mode" >&2
    exit 2
  fi
  while IFS= read -r rust_target; do
    [[ -n "$rust_target" ]] || continue
    artifact="dist-${rust_target}"
    gh run download "$build_run_id" \
      --name "$artifact" \
      --dir "$tmp_dir/raw/$artifact"
  done < <("$root_dir/scripts/release_channel_targets.sh" homebrew official rust_target)
  find "$tmp_dir/raw" -name "plasmite_${version}_*.tar.gz" -print0 | xargs -0 shasum -a 256 > "$tmp_dir/sha256sums.txt"
  load_sha_from_file "$tmp_dir/sha256sums.txt"
  verification_sha_file="$tmp_dir/sha256sums.txt"
else
  echo "Fetching sha256sums from GitHub release ${version_tag}..."
  sums_url="https://github.com/sandover/plasmite/releases/download/${version_tag}/sha256sums.txt"
  curl -fsSL "$sums_url" > "$tmp_dir/sha256sums.txt"
  load_sha_from_file "$tmp_dir/sha256sums.txt"
  verification_sha_file="$tmp_dir/sha256sums.txt"
fi

if [[ "$prepare_only" != true ]]; then
  "$root_dir/scripts/verify_public_release_assets.sh" "$version_tag" "$verification_sha_file"
fi

original_formula="$formula_file"
candidate="$(mktemp "${formula_file}.tmp.XXXXXX")"
cp -p "$formula_file" "$candidate"
formula_file="$candidate"
export version version_tag formula_file

perl -0777 -i.bak -pe '
  s/version "\d+\.\d+\.\d+"/version "$ENV{version}"/g;
  s#/download/v\d+\.\d+\.\d+/#/download/$ENV{version_tag}/#g;
' "$formula_file"

while IFS= read -r platform; do
  [[ -n "$platform" ]] || continue
  sha="$(grep "plasmite_${version}_${platform}.tar.gz" "$verification_sha_file" | awk '{print $1}' | head -n1)"
  placeholder="$(printf '%s' "$platform" | tr '[:lower:]' '[:upper:]')"
  export platform sha placeholder
  perl -0777 -i.bak -pe '
    my $platform = quotemeta($ENV{platform});
    my $placeholder = quotemeta("PLACEHOLDER_" . $ENV{placeholder} . "_SHA256");
    s/plasmite_\d+\.\d+\.\d+_${platform}\.tar\.gz/plasmite_$ENV{version}_$ENV{platform}.tar.gz/g;
    s/$placeholder/$ENV{sha}/g;
    s#(plasmite_$ENV{version}_${platform}\.tar\.gz"\n\s*sha256 ")([^"]+)#$1$ENV{sha}#g;
  ' "$formula_file"
done < <("$root_dir/scripts/release_channel_targets.sh" homebrew official sdk_platform)

rm -f "${formula_file}.bak"

"${root_dir}/scripts/verify_homebrew_formula_alignment.sh" \
  --version "$version" \
  --sha256sums "$verification_sha_file" \
  --formula-file "$formula_file"

if cmp -s "$candidate" "$original_formula"; then
  echo "Formula already aligned: $original_formula"
  exit 0
fi
mv "$candidate" "$original_formula"
echo "Formula updated successfully: $original_formula"
if [[ "$prepare_only" == true ]]; then
  echo "Rehearsal candidate only; do not push. Rerun without --prepare-only after publication."
  exit 0
fi
echo
echo "Next steps:"
echo "  cd ${tap_path}"
echo "  git add Formula/plasmite.rb"
echo "  git commit -m 'plasmite: update to ${version}'"
echo "  git push"
