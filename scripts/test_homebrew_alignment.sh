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

fake_bin="$tmp_dir/bin"
gh_calls="$tmp_dir/gh-calls.txt"
mkdir -p "$fake_bin"
cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" != "run" || "$2" != "download" || "$4" != "--name" || "$6" != "--dir" ]]; then
  echo "unexpected gh arguments: $*" >&2
  exit 2
fi

artifact="$5"
destination="$7"
echo "$artifact" >>"$GH_CALL_LOG"
if [[ "${FAKE_GH_MISSING:-}" == "$artifact" ]]; then
  exit 1
fi

rust_target="${artifact#dist-}"
platform="$(
  jq -r --arg rust_target "$rust_target" '
    .targets[]
    | select(.rust_target == $rust_target and .channels.homebrew == "official")
    | .sdk_platform
  ' "$FAKE_RELEASE_TARGETS"
)"
if [[ -z "$platform" ]]; then
  echo "unexpected artifact requested: $artifact" >&2
  exit 2
fi

mkdir -p "$destination"
printf '%s\n' "$artifact" >"$destination/plasmite_9.9.9_${platform}.tar.gz"
EOF
chmod +x "$fake_bin/gh"

: >"$gh_calls"
PATH="$fake_bin:$PATH" \
  GH_CALL_LOG="$gh_calls" \
  FAKE_RELEASE_TARGETS="$root_dir/release/targets.json" \
  "$root_dir/scripts/update_homebrew_formula.sh" \
  v9.9.9 \
  "$tap_dir" \
  --build-run-id 123 >/dev/null

expected_calls="$tmp_dir/expected-gh-calls.txt"
"$root_dir/scripts/release_channel_targets.sh" homebrew official rust_target |
  sed 's/^/dist-/' >"$expected_calls"
diff -u "$expected_calls" "$gh_calls"

missing_artifact="$(head -n1 "$expected_calls")"
if PATH="$fake_bin:$PATH" \
  GH_CALL_LOG="$gh_calls" \
  FAKE_GH_MISSING="$missing_artifact" \
  FAKE_RELEASE_TARGETS="$root_dir/release/targets.json" \
  "$root_dir/scripts/update_homebrew_formula.sh" \
  v9.9.9 \
  "$tap_dir" \
  --build-run-id 123 >/dev/null 2>&1; then
  echo "expected a missing Homebrew SDK artifact to fail" >&2
  exit 1
fi

echo "manifest-driven Homebrew fixtures ok"
