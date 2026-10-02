#!/usr/bin/env bash
# Purpose: Accept only stable vX.Y.Z release tags.
# Role: Keep release workflow tag handling strict and testable.

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: validate_release_tag.sh <vX.Y.Z>" >&2
  exit 2
fi

tag="$1"
numeric='(0|[1-9][0-9]*)'
release_re="^v${numeric}\\.${numeric}\\.${numeric}$"

if [[ ! "$tag" =~ $release_re ]]; then
  echo "error: release tag must use vX.Y.Z format (got '$tag')." >&2
  exit 1
fi
