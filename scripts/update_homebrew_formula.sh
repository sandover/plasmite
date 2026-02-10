#!/usr/bin/env bash
# Purpose: Update homebrew-tap Formula/plasmite.rb to match a specific release.
# Key exports: Rewrites version, release URLs, and sha256 values for supported targets.
# Role: Keep Homebrew distribution aligned with every Plasmite release.
# Invariants: Requires darwin_amd64, darwin_arm64, and linux_amd64 checksums.
# Invariants: Fails if formula still references unsupported linux_arm64 release artifacts.
# Notes: Checksums can come from an existing GitHub release, a release build run ID, or a local sums file.

set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: update_homebrew_formula.sh <version-tag> <homebrew-tap-path> [--build-run-id <id> | --sha256sums <path>]

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

while [[ $# -gt 0 ]]; do
  case "$1" in
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

if grep -q 'linux_arm64' "$formula_file"; then
  echo "error: formula still references linux_arm64 artifacts, which are not release-gating targets." >&2
  echo "hint: remove linux_arm64 formula entries first, then rerun this script." >&2
  exit 1
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
verification_sha_file=""

load_sha_from_file() {
  local file="$1"
  if [[ ! -f "$file" ]]; then
    echo "error: sha256 file not found: $file" >&2
    exit 1
  fi

  darwin_amd64_sha="$(grep "plasmite_${version}_darwin_amd64.tar.gz" "$file" | awk '{print $1}' | head -n1)"
  darwin_arm64_sha="$(grep "plasmite_${version}_darwin_arm64.tar.gz" "$file" | awk '{print $1}' | head -n1)"
  linux_amd64_sha="$(grep "plasmite_${version}_linux_amd64.tar.gz" "$file" | awk '{print $1}' | head -n1)"

  if [[ -z "$darwin_amd64_sha" || -z "$darwin_arm64_sha" || -z "$linux_amd64_sha" ]]; then
    echo "error: failed to extract required checksums for v${version} from $file" >&2
    echo "  darwin_amd64: ${darwin_amd64_sha:-<missing>}" >&2
    echo "  darwin_arm64: ${darwin_arm64_sha:-<missing>}" >&2
    echo "  linux_amd64: ${linux_amd64_sha:-<missing>}" >&2
    exit 1
  fi
}

if [[ -n "$sha256_file" ]]; then
  load_sha_from_file "$sha256_file"
  verification_sha_file="$sha256_file"
elif [[ -n "$build_run_id" ]]; then
  if ! command -v gh >/dev/null 2>&1; then
    echo "error: gh CLI is required for --build-run-id mode" >&2
    exit 2
  fi
  gh run download "$build_run_id" --dir "$tmp_dir/raw"
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

export version version_tag darwin_amd64_sha darwin_arm64_sha linux_amd64_sha formula_file

perl -0777 -i.bak -pe '
  s/version "\d+\.\d+\.\d+"/version "$ENV{version}"/g;
  s#/download/v\d+\.\d+\.\d+/#/download/$ENV{version_tag}/#g;
  s/plasmite_\d+\.\d+\.\d+_darwin_amd64\.tar\.gz/plasmite_$ENV{version}_darwin_amd64.tar.gz/g;
  s/plasmite_\d+\.\d+\.\d+_darwin_arm64\.tar\.gz/plasmite_$ENV{version}_darwin_arm64.tar.gz/g;
  s/plasmite_\d+\.\d+\.\d+_linux_amd64\.tar\.gz/plasmite_$ENV{version}_linux_amd64.tar.gz/g;
  s/PLACEHOLDER_DARWIN_AMD64_SHA256/$ENV{darwin_amd64_sha}/g;
  s/PLACEHOLDER_DARWIN_ARM64_SHA256/$ENV{darwin_arm64_sha}/g;
  s/PLACEHOLDER_LINUX_AMD64_SHA256/$ENV{linux_amd64_sha}/g;
  s#(plasmite_$ENV{version}_darwin_amd64\.tar\.gz"\n\s*sha256 ")([^"]+)#$1$ENV{darwin_amd64_sha}#g;
  s#(plasmite_$ENV{version}_darwin_arm64\.tar\.gz"\n\s*sha256 ")([^"]+)#$1$ENV{darwin_arm64_sha}#g;
  s#(plasmite_$ENV{version}_linux_amd64\.tar\.gz"\n\s*sha256 ")([^"]+)#$1$ENV{linux_amd64_sha}#g;
' "$formula_file"

rm -f "${formula_file}.bak"

"${root_dir}/scripts/verify_homebrew_formula_alignment.sh" \
  --version "$version" \
  --sha256sums "$verification_sha_file" \
  --formula-file "$formula_file"

echo "Formula updated successfully: $formula_file"
echo
echo "Next steps:"
echo "  cd ${tap_path}"
echo "  git add Formula/plasmite.rb"
echo "  git commit -m 'plasmite: update to ${version}'"
echo "  git push"
