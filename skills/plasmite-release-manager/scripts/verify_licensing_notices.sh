#!/usr/bin/env bash
# Purpose: Verify release artifacts include required licensing and notice files.
# Key outputs: Pass/fail status with per-channel findings.
# Role: Deterministic check for release QA gate 11 and post-release verification.
# Invariants: Fails if found artifacts omit LICENSE or THIRD_PARTY_NOTICES.md.
# Invariants: Exits 2 when no artifacts are available to verify (incomplete evidence).
# Notes: Reads local artifacts only; caller is responsible for downloading release assets.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PY_DIST_DIR="${PY_DIST_DIR:-$ROOT/bindings/python/dist}"
NODE_DIR="${NODE_DIR:-$ROOT/bindings/node}"
RELEASE_DIR="${RELEASE_DIR:-$ROOT/.scratch/release/downloaded}"

failures=0
checked=0

note_fail() {
  echo "FAIL: $*"
  failures=$((failures + 1))
}

note_ok() {
  echo "OK: $*"
}

has_member() {
  local archive="$1"
  local pattern="$2"
  tar -tzf "$archive" | rg -q "$pattern"
}

if [[ -f "$ROOT/LICENSE" && -f "$ROOT/THIRD_PARTY_NOTICES.md" ]]; then
  note_ok "repo-level LICENSE and THIRD_PARTY_NOTICES.md present"
else
  note_fail "missing repo-level LICENSE and/or THIRD_PARTY_NOTICES.md"
fi

if ls "$PY_DIST_DIR"/*.tar.gz >/dev/null 2>&1; then
  for sdist in "$PY_DIST_DIR"/*.tar.gz; do
    checked=$((checked + 1))
    if has_member "$sdist" '/LICENSE$'; then
      note_ok "python sdist includes LICENSE ($(basename "$sdist"))"
    else
      note_fail "python sdist missing LICENSE ($(basename "$sdist"))"
    fi
    if has_member "$sdist" '/THIRD_PARTY_NOTICES\.md$'; then
      note_ok "python sdist includes THIRD_PARTY_NOTICES.md ($(basename "$sdist"))"
    else
      note_fail "python sdist missing THIRD_PARTY_NOTICES.md ($(basename "$sdist"))"
    fi
  done
fi

if ls "$PY_DIST_DIR"/*.whl >/dev/null 2>&1; then
  for wheel in "$PY_DIST_DIR"/*.whl; do
    checked=$((checked + 1))
    if python3 -m zipfile -l "$wheel" | rg -q 'dist-info/(licenses/)?LICENSE'; then
      note_ok "python wheel includes LICENSE metadata ($(basename "$wheel"))"
    else
      note_fail "python wheel missing LICENSE metadata ($(basename "$wheel"))"
    fi
    if python3 -m zipfile -l "$wheel" | rg -q 'THIRD_PARTY_NOTICES\.md'; then
      note_ok "python wheel includes THIRD_PARTY_NOTICES.md ($(basename "$wheel"))"
    else
      note_fail "python wheel missing THIRD_PARTY_NOTICES.md ($(basename "$wheel"))"
    fi
  done
fi

if ls "$NODE_DIR"/*.tgz >/dev/null 2>&1; then
  for tgz in "$NODE_DIR"/*.tgz; do
    checked=$((checked + 1))
    if has_member "$tgz" 'package/LICENSE$'; then
      note_ok "node tarball includes LICENSE ($(basename "$tgz"))"
    else
      note_fail "node tarball missing LICENSE ($(basename "$tgz"))"
    fi
    if has_member "$tgz" 'package/THIRD_PARTY_NOTICES\.md$'; then
      note_ok "node tarball includes THIRD_PARTY_NOTICES.md ($(basename "$tgz"))"
    else
      note_fail "node tarball missing THIRD_PARTY_NOTICES.md ($(basename "$tgz"))"
    fi
  done
fi

if ls "$RELEASE_DIR"/*.tar.gz >/dev/null 2>&1; then
  for sdk in "$RELEASE_DIR"/*.tar.gz; do
    checked=$((checked + 1))
    if has_member "$sdk" '/LICENSE$'; then
      note_ok "release archive includes LICENSE ($(basename "$sdk"))"
    else
      note_fail "release archive missing LICENSE ($(basename "$sdk"))"
    fi
    if has_member "$sdk" '/THIRD_PARTY_NOTICES\.md$'; then
      note_ok "release archive includes THIRD_PARTY_NOTICES.md ($(basename "$sdk"))"
    else
      note_fail "release archive missing THIRD_PARTY_NOTICES.md ($(basename "$sdk"))"
    fi
  done
fi

if [[ "$checked" -eq 0 ]]; then
  echo "INCOMPLETE: no local release artifacts found to verify"
  exit 2
fi

if [[ "$failures" -gt 0 ]]; then
  echo "SUMMARY: $failures licensing/notice checks failed"
  exit 1
fi

echo "SUMMARY: all licensing/notice checks passed"
