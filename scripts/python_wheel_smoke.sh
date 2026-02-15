#!/usr/bin/env bash
# Purpose: Build and smoke-test Python wheel artifacts with bundled native assets.
# Exports: N/A (script entry point).
# Role: CI/local guardrail for Python packaging install-time behavior.
# Invariants: Uses a clean virtual environment for wheel install checks.
# Invariants: Verifies import + bundled CLI execution from installed wheel.
# Invariants: Requires uv for deterministic Python environment/package operations.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/normalize_sdk_layout.sh"
mkdir -p "$ROOT/.scratch"
WORKDIR="$(mktemp -d "$ROOT/.scratch/python-wheel-smoke.XXXXXX")"
UV_CACHE_DIR="$WORKDIR/uv-cache"
HAS_RG=0
if command -v rg >/dev/null 2>&1; then
  HAS_RG=1
fi

if ! command -v uv >/dev/null 2>&1; then
  echo "error: uv is required for python_wheel_smoke.sh." >&2
  echo "hint: install uv and rerun (brew install uv)." >&2
  exit 2
fi

build_env="$WORKDIR/build-env"
install_env="$WORKDIR/install-env"
release_dir="$ROOT/target/release"

resolve_sdk_dir() {
  if [[ -n "${PLASMITE_SDK_DIR:-}" ]]; then
    plasmite_normalize_sdk_dir "$PLASMITE_SDK_DIR" "$WORKDIR/sdk-from-env" "PLASMITE_SDK_DIR"
    return 0
  fi

  if [[ ! -d "$release_dir" ]]; then
    echo "error: release directory not found: $release_dir" >&2
    echo "hint: run 'cargo build --release' before python wheel smoke." >&2
    exit 1
  fi

  local normalized_release_sdk
  if ! normalized_release_sdk="$(plasmite_normalize_sdk_dir "$release_dir" "$WORKDIR/sdk" "Python smoke release SDK")"; then
    echo "hint: build release artifacts that produce libplasmite before python wheel smoke." >&2
    exit 1
  fi

  echo "$normalized_release_sdk"
}

SDK_DIR="$(resolve_sdk_dir)"

wheel_has_member() {
  local wheel="$1"
  local pattern="$2"
  local members
  members="$(python3 -m zipfile -l "$wheel")"
  if [[ "$HAS_RG" -eq 1 ]]; then
    rg -q "$pattern" <<<"$members"
  else
    grep -Eq "$pattern" <<<"$members"
  fi
}

uv venv "$build_env" --cache-dir "$UV_CACHE_DIR"
# shellcheck disable=SC1091
source "$build_env/bin/activate"
if ! uv pip install --cache-dir "$UV_CACHE_DIR" build; then
  echo "error: failed to install python build backend ('build') with uv."
  echo "hint: ensure network access to package indexes (or preinstall build deps) before running python wheel smoke."
  exit 2
fi

(
  cd "$ROOT/bindings/python"
  rm -rf dist
  PLASMITE_SDK_DIR="$SDK_DIR" python -m build
)
deactivate

wheel_file="$(ls -1 "$ROOT"/bindings/python/dist/*.whl | tail -n 1)"
if [[ -z "$wheel_file" ]]; then
  echo "wheel build failed"
  exit 1
fi

wheel_has_member "$wheel_file" 'plasmite/_native/(plasmite\.dll|libplasmite\.(dylib|so))'
wheel_has_member "$wheel_file" 'plasmite/_native/plasmite(\.exe)?'

uv venv "$install_env" --cache-dir "$UV_CACHE_DIR"
# shellcheck disable=SC1091
source "$install_env/bin/activate"
uv pip install --cache-dir "$UV_CACHE_DIR" "$wheel_file"

unset PLASMITE_LIB_DIR DYLD_LIBRARY_PATH LD_LIBRARY_PATH
python -c 'import plasmite; from plasmite import Client; print("python-wheel-import-ok")'
plasmite --version >/dev/null
deactivate

echo "[smoke] python wheel install + runtime ok"
