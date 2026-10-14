#!/usr/bin/env bash
# Verify anonymous public SDK downloads against the selected build's checksums.
set -euo pipefail
if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo 'usage: verify_public_release_assets.sh <tag> <build-sha256sums> [homebrew|github_sdk]' >&2
  exit 2
fi
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"$root_dir/scripts/validate_release_tag.sh" "$1"
tag="$1"
channel="${3:-homebrew}"
case "$channel" in homebrew|github_sdk) ;; *) exit 2 ;; esac
manifest="${PLASMITE_RELEASE_TARGETS:-$root_dir/release/targets.json}"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
base_url="https://github.com/sandover/plasmite/releases/download/$tag"
# No GitHub token or curl configuration: a draft/private asset is not public.
curl --disable -fsSL --retry 2 --connect-timeout 10 --max-time 120 \
  "$base_url/sha256sums.txt" -o "$tmp_dir/public-sums"
python3 - "$manifest" "$tag" "$channel" "$2" "$tmp_dir/public-sums" > "$tmp_dir/expected" <<'PY'
import json, re, sys
from pathlib import Path

manifest, tag, channel, expected_file, public_file = sys.argv[1:]
def checksums(path):
    result = {}
    for line in Path(path).read_text().splitlines():
        parts = line.split()
        if len(parts) != 2 or not re.fullmatch(r'[0-9a-fA-F]{64}', parts[0]):
            raise SystemExit('Invalid checksum record: ' + path)
        name = Path(parts[1].lstrip('*')).name
        if name in result:
            raise SystemExit('Duplicate checksum: ' + name)
        result[name] = parts[0].lower()
    return result

expected, public = checksums(expected_file), checksums(public_file)
targets = [t for t in json.loads(Path(manifest).read_text())['targets']
           if t['channels'].get(channel) == 'official' or
           (channel == 'github_sdk' and t['channels'].get(channel) == 'preview')]
if not targets:
    raise SystemExit('No public SDK targets selected')
for target in targets:
    name = f"plasmite_{tag[1:]}_{target['sdk_platform']}.tar.gz"
    if name not in expected or public.get(name) != expected[name]:
        raise SystemExit('Public checksum differs from selected build: ' + name)
    print(expected[name], name)
PY
while read -r expected name; do
  curl --disable -fsSL --retry 2 --connect-timeout 10 --max-time 120 \
    "$base_url/$name" -o "$tmp_dir/$name"
  actual="$(shasum -a 256 "$tmp_dir/$name" | awk '{print $1}')"
  if [[ "$actual" != "$expected" ]]; then
    echo "error: public SDK differs from selected build: $name" >&2
    exit 1
  fi
done < "$tmp_dir/expected"
echo "Public $tag $channel SDK assets match the selected build."
