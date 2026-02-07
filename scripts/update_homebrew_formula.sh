#!/usr/bin/env bash
# Purpose: Update homebrew-tap formula with sha256 checksums from a release
# Usage: ./scripts/update_homebrew_formula.sh v0.1.0 ../homebrew-tap

set -euo pipefail

if [ $# -ne 2 ]; then
  echo "Usage: $0 <version-tag> <homebrew-tap-path>"
  echo "Example: $0 v0.1.0 ../homebrew-tap"
  exit 1
fi

VERSION_TAG="$1"
TAP_PATH="$2"
VERSION="${VERSION_TAG#v}"

FORMULA_FILE="${TAP_PATH}/Formula/plasmite.rb"

if [ ! -f "$FORMULA_FILE" ]; then
  echo "Error: Formula file not found at $FORMULA_FILE"
  exit 1
fi

echo "Fetching sha256sums from GitHub release ${VERSION_TAG}..."

SUMS_URL="https://github.com/sandover/plasmite/releases/download/${VERSION_TAG}/sha256sums.txt"
SUMS=$(curl -fsSL "$SUMS_URL")

echo "Extracting checksums..."

DARWIN_AMD64_SHA=$(echo "$SUMS" | grep "plasmite_${VERSION}_darwin_amd64.tar.gz" | awk '{print $1}')
DARWIN_ARM64_SHA=$(echo "$SUMS" | grep "plasmite_${VERSION}_darwin_arm64.tar.gz" | awk '{print $1}')
LINUX_AMD64_SHA=$(echo "$SUMS" | grep "plasmite_${VERSION}_linux_amd64.tar.gz" | awk '{print $1}')
LINUX_ARM64_SHA=$(echo "$SUMS" | grep "plasmite_${VERSION}_linux_arm64.tar.gz" | awk '{print $1}')

if [ -z "$DARWIN_AMD64_SHA" ] || [ -z "$DARWIN_ARM64_SHA" ] || [ -z "$LINUX_AMD64_SHA" ] || [ -z "$LINUX_ARM64_SHA" ]; then
  echo "Error: Failed to extract all checksums"
  echo "Got:"
  echo "  darwin_amd64: $DARWIN_AMD64_SHA"
  echo "  darwin_arm64: $DARWIN_ARM64_SHA"
  echo "  linux_amd64: $LINUX_AMD64_SHA"
  echo "  linux_arm64: $LINUX_ARM64_SHA"
  exit 1
fi

echo "Updating formula..."

# Update version
sed -i.bak "s/version \".*\"/version \"${VERSION}\"/" "$FORMULA_FILE"

# Update URLs
sed -i.bak "s|/v[0-9.]*/plasmite_[0-9.]*_darwin_amd64.tar.gz|/${VERSION_TAG}/plasmite_${VERSION}_darwin_amd64.tar.gz|g" "$FORMULA_FILE"
sed -i.bak "s|/v[0-9.]*/plasmite_[0-9.]*_darwin_arm64.tar.gz|/${VERSION_TAG}/plasmite_${VERSION}_darwin_arm64.tar.gz|g" "$FORMULA_FILE"
sed -i.bak "s|/v[0-9.]*/plasmite_[0-9.]*_linux_amd64.tar.gz|/${VERSION_TAG}/plasmite_${VERSION}_linux_amd64.tar.gz|g" "$FORMULA_FILE"
sed -i.bak "s|/v[0-9.]*/plasmite_[0-9.]*_linux_arm64.tar.gz|/${VERSION_TAG}/plasmite_${VERSION}_linux_arm64.tar.gz|g" "$FORMULA_FILE"

# Update checksums
sed -i.bak "s/PLACEHOLDER_DARWIN_AMD64_SHA256/${DARWIN_AMD64_SHA}/" "$FORMULA_FILE"
sed -i.bak "s/sha256 \"[a-f0-9]*\" # darwin_amd64/sha256 \"${DARWIN_AMD64_SHA}\"/" "$FORMULA_FILE"

sed -i.bak "s/PLACEHOLDER_DARWIN_ARM64_SHA256/${DARWIN_ARM64_SHA}/" "$FORMULA_FILE"
sed -i.bak "s/sha256 \"[a-f0-9]*\" # darwin_arm64/sha256 \"${DARWIN_ARM64_SHA}\"/" "$FORMULA_FILE"

sed -i.bak "s/PLACEHOLDER_LINUX_AMD64_SHA256/${LINUX_AMD64_SHA}/" "$FORMULA_FILE"
sed -i.bak "s/sha256 \"[a-f0-9]*\" # linux_amd64/sha256 \"${LINUX_AMD64_SHA}\"/" "$FORMULA_FILE"

sed -i.bak "s/PLACEHOLDER_LINUX_ARM64_SHA256/${LINUX_ARM64_SHA}/" "$FORMULA_FILE"
sed -i.bak "s/sha256 \"[a-f0-9]*\" # linux_arm64/sha256 \"${LINUX_ARM64_SHA}\"/" "$FORMULA_FILE"

rm -f "${FORMULA_FILE}.bak"

echo "Formula updated successfully!"
echo ""
echo "Next steps:"
echo "  cd ${TAP_PATH}"
echo "  git add Formula/plasmite.rb"
echo "  git commit -m 'plasmite: update to ${VERSION}'"
echo "  git push"
