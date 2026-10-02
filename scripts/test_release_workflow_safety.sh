#!/usr/bin/env bash
# Purpose: Check release tag validation, immutable tag resolution, and workflow pins.
# Role: Catch regressions in release input safety and source SHA propagation.

set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
validate_tag="$root_dir/scripts/validate_release_tag.sh"
resolve_tag="$root_dir/scripts/resolve_release_tag_sha.sh"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

for tag in \
  v0.0.0 \
  v1.2.3; do
  "$validate_tag" "$tag"
done

for tag in \
  "" \
  1.2.3 \
  v1.2 \
  v01.2.3 \
  v1.02.3 \
  v1.2.03 \
  v1.2.3-01 \
  v1.2.3-rc.1 \
  v1.2.3+build.7 \
  v1.2.3-alpha..1 \
  v1.2.3+build..7 \
  v1.2.3/unsafe; do
  if "$validate_tag" "$tag" >/dev/null 2>&1; then
    echo "error: release tag validator accepted '$tag'." >&2
    exit 1
  fi
done

mkdir -p "$tmp_dir/bin"
cat > "$tmp_dir/bin/git" <<'GIT'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == "ls-remote" ]] || exit 90
case "${*: -1}" in
  *v2.3.4\^\{\})
    printf '%s\t%s\n' \
      aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa refs/tags/v2.3.4 \
      bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 'refs/tags/v2.3.4^{}' \
      cccccccccccccccccccccccccccccccccccccccc refs/tags/v2.3.40
    ;;
  *v2.3.5\^\{\})
    printf '%s\t%s\n' dddddddddddddddddddddddddddddddddddddddd refs/tags/v2.3.5
    ;;
  *) exit 2 ;;
esac
GIT
chmod +x "$tmp_dir/bin/git"

annotated_sha="$(PATH="$tmp_dir/bin:$PATH" "$resolve_tag" sandover/plasmite v2.3.4)"
[[ "$annotated_sha" == bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ]]
lightweight_sha="$(PATH="$tmp_dir/bin:$PATH" "$resolve_tag" sandover/plasmite v2.3.5)"
[[ "$lightweight_sha" == dddddddddddddddddddddddddddddddddddddddd ]]
if PATH="$tmp_dir/bin:$PATH" "$resolve_tag" sandover/plasmite v2.3.6 >/dev/null 2>&1; then
  echo "error: tag resolver accepted a missing tag." >&2
  exit 1
fi

# A SHA-only checkout has no tag, so Plasmite would report a development
# version. Fetching the exact release tag restores its release identity.
git init -q "$tmp_dir/source"
git -C "$tmp_dir/source" config user.email release-test@example.com
git -C "$tmp_dir/source" config user.name 'Release test'
echo source > "$tmp_dir/source/source"
git -C "$tmp_dir/source" add source
git -C "$tmp_dir/source" -c commit.gpgsign=false commit -qm 'Add source'
git -C "$tmp_dir/source" -c tag.gpgsign=false tag v1.2.3
source_sha="$(git -C "$tmp_dir/source" rev-parse HEAD)"
git clone -q --bare "$tmp_dir/source" "$tmp_dir/remote.git"
git init -q "$tmp_dir/build"
git -C "$tmp_dir/build" remote add origin "file://$tmp_dir/remote.git"
git -C "$tmp_dir/build" fetch -q --no-tags --depth=1 origin "$source_sha"
git -C "$tmp_dir/build" checkout -q --detach FETCH_HEAD
cp "$root_dir/build_version.rs" "$tmp_dir/build_version.rs"
cat > "$tmp_dir/check_build_identity.rs" <<'RUST'
mod build_version;

fn main() {
    let checkout = std::env::args().nth(1).expect("checkout path");
    println!("{}", build_version::build_identity(std::path::Path::new(&checkout), "1.2.3"));
}
RUST
rustc --edition=2021 "$tmp_dir/check_build_identity.rs" -o "$tmp_dir/check_build_identity"
identity_before="$("$tmp_dir/check_build_identity" "$tmp_dir/build" | tail -n 1)"
[[ "$identity_before" == 1.2.3-dev+g* ]]
git -C "$tmp_dir/build" fetch -q --no-tags origin refs/tags/v1.2.3:refs/tags/v1.2.3
[[ "$(git -C "$tmp_dir/build" rev-parse HEAD)" == "$source_sha" ]]
[[ "$(git -C "$tmp_dir/build" rev-parse 'refs/tags/v1.2.3^{commit}')" == "$source_sha" ]]
identity_after="$("$tmp_dir/check_build_identity" "$tmp_dir/build" | tail -n 1)"
[[ "$identity_after" == 1.2.3 ]]

python3 - "$root_dir" <<'PY'
from pathlib import Path
import sys

root = Path(sys.argv[1])
release = (root / ".github/workflows/release.yml").read_text()
publish = (root / ".github/workflows/release-publish.yml").read_text()

assert "RELEASE_TAG_INPUT: ${{ inputs.tag }}" in release
assert 'tag="${{ inputs.tag }}"' not in release
assert "source_sha: ${{ steps.ctx.outputs.source_sha }}" in release
assert 'ref: ${{ needs.release-context.outputs.tag }}' not in release
assert "--arg source_sha \"$RELEASE_SOURCE_SHA\"" in release
assert '"$GITHUB_EVENT_NAME" == "push" && "$GITHUB_SHA" != "$source_sha"' in release
assert 'git fetch --no-tags origin "refs/tags/${RELEASE_TAG}:refs/tags/${RELEASE_TAG}"' in release
assert 'test "$(git rev-parse "refs/tags/${RELEASE_TAG}^{commit}")" = "$RELEASE_SOURCE_SHA"' in release
assert release.index("Restore and verify release tag for build identity") < release.index("Build native artifacts once")

assert "RELEASE_BUILD_RUN_ID_INPUT: ${{ inputs.build_run_id }}" in publish
assert "RELEASE_TAG_INPUT: ${{ inputs.release_tag }}" in publish
assert 'requested_build_run_id="${{ inputs.build_run_id }}"' not in publish
assert 'requested_release_tag="${{ inputs.release_tag }}"' not in publish
assert "source_sha: ${{ steps.resolve.outputs.source_sha }}" in publish
assert "tag_source_sha=\"$(./scripts/resolve_release_tag_sha.sh" in publish
assert '"$source_sha" != "$tag_source_sha"' in publish
assert '"$requested_build_run_id" =~ ^[1-9][0-9]*$' in publish
assert 'selected_run_dir="$tmp_dir/selected-run"' in publish
assert 'ref: ${{ needs.resolve-build-run.outputs.tag }}' not in publish
assert "source_sha=$source_sha" in publish

def checkout_blocks(text):
    lines = text.splitlines()
    blocks = []
    for index, line in enumerate(lines):
        if line.strip() != "- uses: actions/checkout@v4":
            continue
        indent = len(line) - len(line.lstrip())
        block = [line]
        for following in lines[index + 1 :]:
            if following.strip() and len(following) - len(following.lstrip()) == indent and following.lstrip().startswith("-"):
                break
            block.append(following)
        blocks.append("\n".join(block))
    return blocks

release_checkout_refs = [
    next(line.strip().removeprefix("ref:").strip() for line in block.splitlines() if "ref:" in line)
    for block in checkout_blocks(release)
]
assert release_checkout_refs == [
    "${{ github.sha }}",
    "${{ steps.ctx.outputs.source_sha }}",
    "${{ needs.release-context.outputs.source_sha }}",
    "${{ needs.release-context.outputs.source_sha }}",
]

publish_checkout_refs = []
for block in checkout_blocks(publish):
    if "repository: ${{ env.HOMEBREW_TAP_REPO }}" in block:
        continue
    publish_checkout_refs.append(
        next(line.strip().removeprefix("ref:").strip() for line in block.splitlines() if "ref:" in line)
    )
assert publish_checkout_refs == [
    "${{ github.sha }}",
    "${{ needs.resolve-build-run.outputs.source_sha }}",
    "${{ needs.resolve-build-run.outputs.source_sha }}",
    "${{ needs.resolve-build-run.outputs.source_sha }}",
    "${{ needs.resolve-build-run.outputs.source_sha }}",
]

print("release workflow safety checks passed")
PY
