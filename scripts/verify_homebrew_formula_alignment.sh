#!/usr/bin/env bash
# Purpose: Validate Homebrew formula release alignment against SDK artifact checksums.
# Key exports: Exit 0 on exact match; prints actionable mismatch diagnostics on failure.
# Role: Fail-closed guard to keep Homebrew distribution aligned with every release.
# Invariants: Formula version must equal the release version under validation.
# Invariants: Formula URLs and sha256 entries for darwin_amd64/darwin_arm64/linux_amd64/linux_arm64 must match.
# Notes: Reads formula from a local file or directly from GitHub via gh api.

set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: verify_homebrew_formula_alignment.sh --version <X.Y.Z> --sha256sums <path> [--formula-file <path> | --tap-repo <owner/repo>]

examples:
  scripts/verify_homebrew_formula_alignment.sh --version 0.1.9 --sha256sums .scratch/release/sha256sums.txt
  scripts/verify_homebrew_formula_alignment.sh --version 0.1.9 --sha256sums .scratch/release/sha256sums.txt --formula-file ../homebrew-tap/Formula/plasmite.rb
USAGE
}

version=""
sha_file=""
formula_file=""
tap_repo="sandover/homebrew-tap"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      version="${2:-}"
      shift 2
      ;;
    --sha256sums)
      sha_file="${2:-}"
      shift 2
      ;;
    --formula-file)
      formula_file="${2:-}"
      shift 2
      ;;
    --tap-repo)
      tap_repo="${2:-}"
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

if [[ -z "$version" || -z "$sha_file" ]]; then
  echo "error: --version and --sha256sums are required" >&2
  usage
  exit 2
fi

if [[ ! -f "$sha_file" ]]; then
  echo "error: sha256 file not found: $sha_file" >&2
  exit 1
fi

if [[ -n "$formula_file" && ! -f "$formula_file" ]]; then
  echo "error: formula file not found: $formula_file" >&2
  exit 1
fi

if [[ -n "$formula_file" ]]; then
  formula_text="$(cat "$formula_file")"
else
  if ! command -v gh >/dev/null 2>&1; then
    echo "error: gh CLI is required when --formula-file is not provided" >&2
    exit 2
  fi
  formula_text="$(gh api "repos/${tap_repo}/contents/Formula/plasmite.rb" -H "Accept: application/vnd.github.raw")"
fi

formula_version="$(sed -n 's/^[[:space:]]*version[[:space:]]*"\([^"]*\)".*/\1/p' <<<"$formula_text" | head -n1)"
if [[ "$formula_version" != "$version" ]]; then
  echo "error: Homebrew formula version mismatch (formula=$formula_version expected=$version)." >&2
  exit 1
fi

tag="v${version}"
errors=0

expected_sha() {
  local platform="$1"
  grep "plasmite_${version}_${platform}.tar.gz" "$sha_file" | awk '{print $1}' | head -n1
}

extract_formula_entry() {
  local platform="$1"
  awk -v platform="$platform" '
    $0 ~ /url "/ && $0 ~ ("plasmite_[0-9.]+_" platform "\\.tar\\.gz") {capture=1; print; next}
    capture && $0 ~ /sha256 "/ {print; exit}
  ' <<<"$formula_text"
}

for platform in darwin_amd64 darwin_arm64 linux_amd64 linux_arm64; do
  expected="$(expected_sha "$platform")"
  if [[ -z "$expected" ]]; then
    echo "error: missing expected checksum for ${platform} in ${sha_file}" >&2
    errors=1
    continue
  fi

  entry="$(extract_formula_entry "$platform")"
  formula_url="$(sed -n '1s/^[[:space:]]*url[[:space:]]*"\([^"]*\)".*/\1/p' <<<"$entry")"
  formula_sha="$(sed -n '2s/^[[:space:]]*sha256[[:space:]]*"\([^"]*\)".*/\1/p' <<<"$entry")"

  expected_url="https://github.com/sandover/plasmite/releases/download/${tag}/plasmite_${version}_${platform}.tar.gz"
  if [[ "$formula_url" != "$expected_url" ]]; then
    echo "error: formula URL mismatch for ${platform}" >&2
    echo "  got:      ${formula_url:-<missing>}" >&2
    echo "  expected: $expected_url" >&2
    errors=1
  fi
  if [[ "$formula_sha" != "$expected" ]]; then
    echo "error: formula sha256 mismatch for ${platform}" >&2
    echo "  got:      ${formula_sha:-<missing>}" >&2
    echo "  expected: $expected" >&2
    errors=1
  fi
done

if [[ "$errors" -ne 0 ]]; then
  exit 1
fi

echo "ok: Homebrew formula is aligned to v${version}"
