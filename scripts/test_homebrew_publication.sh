#!/usr/bin/env bash
# Exercise the actual release step and tap helper against isolated fake downloads.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/build" "$scratch/public" "$scratch/tap/Formula" "$scratch/runtime/release-inputs/dist"
export FIXTURE="$scratch" TRACE="$scratch/trace" GITHUB_REPOSITORY=sandover/plasmite TAG=v9.9.9
export PLASMITE_RELEASE_TARGETS="$root/release/targets.json"
while read -r platform; do
  file="plasmite_9.9.9_${platform}.tar.gz"
  printf 'SDK %s\n' "$platform" > "$scratch/build/$file"
  (cd "$scratch/build" && shasum -a 256 "$file") >> "$scratch/build/sha256sums.txt"
done < <(jq -r '.targets[]|select(.channels.github_sdk!=null)|.sdk_platform' "$PLASMITE_RELEASE_TARGETS")
{
  echo 'class Plasmite < Formula'
  echo '  version "1.2.1"'
  while read -r platform; do
    echo "  url \"https://github.com/sandover/plasmite/releases/download/v1.2.1/plasmite_1.2.1_${platform}.tar.gz\""
    echo '  sha256 "old"'
  done < <("$root/scripts/release_channel_targets.sh" homebrew)
  echo end
} > "$scratch/original.rb"
cp "$scratch/build/"* "$scratch/runtime/release-inputs/dist/"
echo 'Fixture notes' > "$scratch/runtime/changelog-section.md"
cat > "$scratch/bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "curl $*" >> "$TRACE"
url=""; output=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o) output="$2"; shift 2 ;;
    --retry|--connect-timeout|--max-time) shift 2 ;;
    https://*) url="$1"; shift ;;
    -H|--header|-u|--user) exit 91 ;;
    *) shift ;;
  esac
done
[[ "$url" == https://github.com/sandover/plasmite/releases/download/v9.9.9/* ]] || exit 92
file="$FIXTURE/public/${url##*/}"
[[ -f "$file" && "${SCENARIO:-}" != draft ]] || exit 22
if [[ -n "$output" ]]; then cp "$file" "$output"; else cat "$file"; fi
SH
cat > "$scratch/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "gh $*" >> "$TRACE"
if [[ "$1" == api ]]; then
  case "$SCENARIO" in
    absent) echo 'HTTP 404: Not Found' >&2; exit 1 ;;
    auth) echo 'HTTP 403: Forbidden' >&2; exit 1 ;;
    draft) echo '{"tag_name":"v9.9.9","draft":true,"prerelease":false}' ;;
    *) echo '{"tag_name":"v9.9.9","draft":false,"prerelease":false}' ;;
  esac
elif [[ "$1 $2" == 'release create' ]]; then
  [[ "$SCENARIO" == absent ]] || exit 93
  cp "$FIXTURE/build/"* "$FIXTURE/public/"
else
  exit 94
fi
SH
chmod +x "$scratch/bin/"*
export PATH="$scratch/bin:$PATH"
# Run the real workflow's creation/recovery code, including its heredoc.
python3 - "$root" "$scratch" <<'PY'
from pathlib import Path
import sys, textwrap
root, scratch = map(Path, sys.argv[1:])
workflow = (root / '.github/workflows/release-publish.yml').read_text()
step = workflow.split('      - name: Create GitHub Release\n', 1)[1].split('      - name: Verify public SDK', 1)[0]
script = textwrap.dedent(step.split('        run: |\n', 1)[1])
script = script.replace('/tmp/', str(scratch / 'runtime') + '/')
script = script.replace('${{ github.repository }}', 'sandover/plasmite')
(scratch / 'create.sh').write_text(script)
# Publication cannot start before all registry jobs succeed.
release = workflow.split('  release:\n', 1)[1]
assert 'publish-pypi, publish-crates-io, publish-npm]' in release
assert 'if: always()' not in release.split('    steps:', 1)[0]
assert 'sync-homebrew-tap' not in workflow
assert 'ref: ${{ github.sha }}' in release
PY
reset() {
  cp "$scratch/original.rb" "$scratch/tap/Formula/plasmite.rb"
  rm -f "$scratch/public/"*
  : > "$TRACE"
}
update() { "$root/scripts/update_homebrew_formula.sh" "$TAG" "$scratch/tap" --sha256sums "$scratch/build/sha256sums.txt"; }
reject_update() {
  if update > "$scratch/output" 2>&1; then echo 'Unsafe tap update succeeded' >&2; exit 1; fi
  cmp "$scratch/original.rb" "$scratch/tap/Formula/plasmite.rb"
}
# Before publication / failed registry stage: not one formula byte changes.
reset
reject_update
# Rehearsal renders only a local candidate, with no public requests.
: > "$TRACE"
"$root/scripts/update_homebrew_formula.sh" "$TAG" "$scratch/tap" --sha256sums "$scratch/build/sha256sums.txt" --prepare-only > "$scratch/prepare-output"
grep -q 'do not push' "$scratch/prepare-output"
[[ ! -s "$TRACE" ]]
mkdir -p "$scratch/git-tap/Formula"
cp "$scratch/original.rb" "$scratch/git-tap/Formula/plasmite.rb"
git -C "$scratch/git-tap" init -q
if "$root/scripts/update_homebrew_formula.sh" "$TAG" "$scratch/git-tap" --sha256sums "$scratch/build/sha256sums.txt" --prepare-only > "$scratch/output" 2>&1; then exit 1; fi
cmp "$scratch/original.rb" "$scratch/git-tap/Formula/plasmite.rb"
reset
export SCENARIO=auth
if (cd "$scratch/runtime" && bash "$scratch/create.sh") > "$scratch/output" 2>&1; then exit 1; fi
! grep -q 'gh release create' "$TRACE"
reject_update
# Normal publication makes verified downloads available before the tap advances.
reset
export SCENARIO=absent
(cd "$scratch/runtime" && bash "$scratch/create.sh") >/dev/null
"$root/scripts/verify_public_release_assets.sh" "$TAG" "$scratch/build/sha256sums.txt" github_sdk >/dev/null
update >/dev/null
cp "$scratch/tap/Formula/plasmite.rb" "$scratch/aligned.rb"
update >/dev/null
cmp "$scratch/aligned.rb" "$scratch/tap/Formula/plasmite.rb"
# Retry preserves existing release notes/assets instead of calling create again.
: > "$TRACE"
export SCENARIO=existing
(cd "$scratch/runtime" && bash "$scratch/create.sh") >/dev/null
! grep -q 'gh release create' "$TRACE"
"$root/scripts/verify_public_release_assets.sh" "$TAG" "$scratch/build/sha256sums.txt" github_sdk >/dev/null
export SCENARIO=draft
if (cd "$scratch/runtime" && bash "$scratch/create.sh") > "$scratch/output" 2>&1; then exit 1; fi
! grep -q 'gh release create' "$TRACE"
# Missing, corrupt, mismatched-checksum and draft assets all block promotion.
first="$("$root/scripts/release_channel_targets.sh" homebrew | head -n1)"
file="plasmite_9.9.9_${first}.tar.gz"
for failure in missing corrupt checksum draft; do
  reset
  cp "$scratch/build/"* "$scratch/public/"
  export SCENARIO=existing
  case "$failure" in
    missing) rm "$scratch/public/$file" ;;
    corrupt) echo corrupt > "$scratch/public/$file" ;;
    checksum) awk 'NR==1{$1="0000000000000000000000000000000000000000000000000000000000000000"}{print}' "$scratch/build/sha256sums.txt" > "$scratch/public/sha256sums.txt" ;;
    draft) export SCENARIO=draft ;;
  esac
  reject_update
done
# Duplicated trusted checksums must not pick an arbitrary first match.
reset
cp "$scratch/build/"* "$scratch/public/"
cp "$scratch/build/sha256sums.txt" "$scratch/duplicate-sums"
head -n1 "$scratch/duplicate-sums" >> "$scratch/duplicate-sums-copy"
cat "$scratch/duplicate-sums-copy" >> "$scratch/duplicate-sums"
if "$root/scripts/update_homebrew_formula.sh" "$TAG" "$scratch/tap" --sha256sums "$scratch/duplicate-sums" > "$scratch/output" 2>&1; then exit 1; fi
cmp "$scratch/original.rb" "$scratch/tap/Formula/plasmite.rb"
# A partial existing release is recovered by restoring only its missing verified asset.
reset
cp "$scratch/build/"* "$scratch/public/"
rm "$scratch/public/$file"
export SCENARIO=existing
(cd "$scratch/runtime" && bash "$scratch/create.sh") >/dev/null
reject_update
cp "$scratch/build/$file" "$scratch/public/$file"
update >/dev/null
# Malformed candidate template never replaces the original, even after verification.
echo 'not a formula' > "$scratch/tap/Formula/plasmite.rb"
if update > "$scratch/output" 2>&1; then exit 1; fi
[[ "$(cat "$scratch/tap/Formula/plasmite.rb")" == 'not a formula' ]]
cp "$scratch/original.rb" "$scratch/tap/Formula/plasmite.rb"
echo '  url "https://example.com/unverified.tar.gz"' >> "$scratch/tap/Formula/plasmite.rb"
cp "$scratch/tap/Formula/plasmite.rb" "$scratch/extra-url.rb"
if update > "$scratch/output" 2>&1; then exit 1; fi
cmp "$scratch/extra-url.rb" "$scratch/tap/Formula/plasmite.rb"
echo 'Homebrew publication ordering / failed and resumed publication fixtures passed.'
