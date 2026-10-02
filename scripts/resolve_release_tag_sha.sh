#!/usr/bin/env bash
# Purpose: Resolve a valid release tag to the commit it currently names.
# Role: Pin release builds and validate publish provenance against the tag.

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: resolve_release_tag_sha.sh <owner/repository> <vX.Y.Z>" >&2
  exit 2
fi

repository="$1"
tag="$2"
if [[ ! "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "error: repository must use owner/repository form." >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
"$script_dir/validate_release_tag.sh" "$tag"

tag_ref="refs/tags/${tag}"
if ! remote_refs="$(git ls-remote --exit-code --tags \
  "https://github.com/${repository}.git" \
  "$tag_ref" "${tag_ref}^{}")"; then
  echo "error: release tag '${tag}' does not exist on origin." >&2
  exit 1
fi

source_sha="$(awk -v tag_ref="$tag_ref" -v peeled_ref="${tag_ref}^{}" '
  $2 == peeled_ref { peeled_sha = $1 }
  $2 == tag_ref { direct_sha = $1 }
  END { print (peeled_sha != "" ? peeled_sha : direct_sha) }
' <<<"$remote_refs")"

if [[ ! "$source_sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: origin did not resolve '${tag}' to a commit SHA." >&2
  exit 1
fi

printf '%s\n' "$source_sha"
