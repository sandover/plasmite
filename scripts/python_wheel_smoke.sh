#!/usr/bin/env bash
# Purpose: Build and smoke-test Python wheel artifacts with bundled native assets.
# Exports: N/A (script entry point).
# Role: CI/local guardrail for Python packaging install-time behavior.
# Invariants: Uses a clean virtual environment for wheel install checks.
# Invariants: Verifies import + bundled CLI execution from installed wheel.
# Notes: Prefers uv for env/package management; falls back to pip when uv is unavailable.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SDK_DIR="${PLASMITE_SDK_DIR:-$ROOT/target/release}"
WORKDIR="$(mktemp -d "$ROOT/.scratch/python-wheel-smoke.XXXXXX")"
UV_CACHE_DIR="$WORKDIR/uv-cache"
PIP_CACHE_DIR="$WORKDIR/pip-cache"

build_env="$WORKDIR/build-env"
install_env="$WORKDIR/install-env"

if command -v uv >/dev/null 2>&1; then
  uv venv "$build_env" --cache-dir "$UV_CACHE_DIR"
  # shellcheck disable=SC1091
  source "$build_env/bin/activate"
  if ! uv pip install --cache-dir "$UV_CACHE_DIR" build; then
    echo "error: failed to install python build backend ('build') with uv."
    echo "hint: ensure network access to package indexes (or preinstall build deps) before running python wheel smoke."
    exit 2
  fi
else
  python3 -m venv "$build_env"
  # shellcheck disable=SC1091
  source "$build_env/bin/activate"
  if ! PIP_CACHE_DIR="$PIP_CACHE_DIR" python -m pip install build; then
    echo "error: failed to install python build backend ('build') with pip."
    echo "hint: ensure network access to package indexes (or preinstall build deps) before running python wheel smoke."
    exit 2
  fi
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

python3 -m zipfile -l "$wheel_file" | rg -q 'plasmite/_native/libplasmite\.(dylib|so)'
python3 -m zipfile -l "$wheel_file" | rg -q 'plasmite/_native/plasmite'

if command -v uv >/dev/null 2>&1; then
  uv venv "$install_env" --cache-dir "$UV_CACHE_DIR"
  # shellcheck disable=SC1091
  source "$install_env/bin/activate"
  uv pip install --cache-dir "$UV_CACHE_DIR" "$wheel_file"
else
  python3 -m venv "$install_env"
  # shellcheck disable=SC1091
  source "$install_env/bin/activate"
  PIP_CACHE_DIR="$PIP_CACHE_DIR" python -m pip install "$wheel_file"
fi

unset PLASMITE_LIB_DIR DYLD_LIBRARY_PATH LD_LIBRARY_PATH
python -c 'import plasmite; from plasmite import Client; print("python-wheel-import-ok")'
plasmite --version >/dev/null
deactivate

echo "[smoke] python wheel install + runtime ok"
