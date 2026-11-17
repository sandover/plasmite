#!/usr/bin/env bash
# Purpose: Build and smoke-test Python wheel artifacts with bundled native assets.
# Exports: N/A (script entry point).
# Role: CI/local guardrail for Python packaging install-time behavior.
# Invariants: Uses a clean virtual environment for wheel install checks.
# Invariants: Verifies import + bundled CLI execution and secure server restart from the wheel.
# Invariants: Requires uv for deterministic Python environment/package operations.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/normalize_sdk_layout.sh"
mkdir -p "$ROOT/.scratch"
WORKDIR="$(mktemp -d "$ROOT/.scratch/python-wheel-smoke.XXXXXX")"
cleanup_workdir() {
  local exit_status=$?
  if [[ "$exit_status" -eq 0 ]]; then
    rm -rf "$WORKDIR"
  fi
}
trap cleanup_workdir EXIT
export UV_CACHE_DIR="${UV_CACHE_DIR:-$WORKDIR/uv-cache}"
HAS_RG=0
if command -v rg >/dev/null 2>&1; then
  HAS_RG=1
fi

if ! command -v uv >/dev/null 2>&1; then
  echo "error: uv is required for python_wheel_smoke.sh." >&2
  echo "hint: install uv and rerun (brew install uv)." >&2
  exit 2
fi
WHEEL_SMOKE_PYTHON="$(command -v python || command -v python3 || true)"
if [[ -z "$WHEEL_SMOKE_PYTHON" ]]; then
  echo "error: Python 3 is required for python_wheel_smoke.sh." >&2
  exit 2
fi

build_env="$WORKDIR/build-env"
install_env="$WORKDIR/install-env"
release_dir="$ROOT/target/release"

activate_venv() {
  local venv_dir="$1"
  local activate_sh="$venv_dir/bin/activate"
  if [[ ! -f "$activate_sh" ]]; then
    activate_sh="$venv_dir/Scripts/activate"
  fi
  if [[ ! -f "$activate_sh" ]]; then
    echo "error: virtualenv activation script not found under '$venv_dir'." >&2
    exit 1
  fi
  # shellcheck disable=SC1090
  source "$activate_sh"
}

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
  members="$("$WHEEL_SMOKE_PYTHON" -m zipfile -l "$wheel")"
  if [[ "$HAS_RG" -eq 1 ]]; then
    rg -q "$pattern" <<<"$members"
  else
    grep -Eq "$pattern" <<<"$members"
  fi
}

# Build every wheel from the published source archive, outside the checkout.
# CPython 3.11+ shares abi3; 3.10 needs its own buffer-protocol extension.
# The explicit ctypes wheel also serves PyPy and free-threaded Python.
mkdir -p "$WORKDIR/sdist" "$ROOT/bindings/python/dist"
rm -f "$ROOT"/bindings/python/dist/*.whl "$ROOT"/bindings/python/dist/*.tar.gz
PLASMITE_SDK_DIR="$SDK_DIR" uv build --sdist --python 3.11 \
  --out-dir "$WORKDIR/sdist" "$ROOT/bindings/python"
sdist_file="$(ls -1 "$WORKDIR"/sdist/*.tar.gz)"
cp "$sdist_file" "$ROOT/bindings/python/dist/"

for variant in abi3 cp310 fallback; do
  case "$variant" in
    abi3) build_python=3.11; view_mode=1; expected_backend=native ;;
    cp310) build_python=3.10; view_mode=1; expected_backend=native ;;
    fallback) build_python=3.11; view_mode=0; expected_backend=python ;;
  esac
  wheel_dir="$WORKDIR/wheels-$variant"
  PLASMITE_SDK_DIR="$SDK_DIR" PLASMITE_PYTHON_VIEWS="$view_mode" \
    uv build --wheel --python "$build_python" --out-dir "$wheel_dir" "$sdist_file"
  wheel_file="$(ls -1 "$wheel_dir"/*.whl)"
  if [[ -n "${PLASMITE_MANYLINUX_PLATFORM:-}" ]]; then
    uv venv "$build_env" --python 3.11 --cache-dir "$UV_CACHE_DIR" --allow-existing
    activate_venv "$build_env"
    uv pip install --cache-dir "$UV_CACHE_DIR" 'auditwheel==6.8.2' 'patchelf==0.19.1.0'
    auditwheel repair --plat "$PLASMITE_MANYLINUX_PLATFORM" --only-plat \
      --wheel-dir "$WORKDIR/repaired-$variant" "$wheel_file"
    deactivate
    wheel_file="$(ls -1 "$WORKDIR/repaired-$variant"/*.whl)"
  fi
  cp "$wheel_file" "$ROOT/bindings/python/dist/"
  wheel_has_member "$wheel_file" 'plasmite/_native/(plasmite2\.dll|libplasmite\.(2\.dylib|so\.2))'
  wheel_has_member "$wheel_file" 'plasmite/_native/plasmite(\.exe)?'
  uv run --no-project --python "$build_python" --with packaging \
    python "$ROOT/scripts/verify_python_wheel_platform.py" "$wheel_file" "$SDK_DIR"
  uv venv "$install_env-$variant" --python "$build_python" --cache-dir "$UV_CACHE_DIR"
  activate_venv "$install_env-$variant"
  uv pip install --cache-dir "$UV_CACHE_DIR" "$wheel_file"
  unset PLASMITE_LIB_DIR DYLD_LIBRARY_PATH LD_LIBRARY_PATH PLASMITE_PYTHON_VIEWS
  EXPECTED_VIEW_BACKEND="$expected_backend" python - <<'PY'
import gc
import json
import os
from collections.abc import Mapping, Sequence
from tempfile import TemporaryDirectory
from plasmite import Client, to_json, to_native, view_from_lite3
from plasmite._views import VIEW_BACKEND

assert VIEW_BACKEND == os.environ["EXPECTED_VIEW_BACKEND"], VIEW_BACKEND
with TemporaryDirectory() as directory, Client(directory) as client:
    with client.open_pool("wheel-smoke", create=True) as pool:
        sent = pool.feed({"wheel": True, "values": [1, 2, 3]}, ["package"])
        with pool.fetch(since=sent.seq) as reader:
            received = next(reader)
        assert received.data["wheel"] and received.tags == ["package"]
        with pool.fetch_view(since=sent.seq) as reader:
            view = next(reader)
        assert isinstance(view.data, Mapping) and isinstance(view.data["values"], Sequence)
        assert to_native(view)["data"] == received.data
        assert json.loads(to_json(view))["data"] == received.data
        child, buffer = view.data["values"], memoryview(view)
        assert buffer.readonly
        assert view_from_lite3(bytes(buffer)).data == received.data
        produced = pool.feed_view({"integer": (1 << 63) - 1, "bytes": b"copy"}, ["nul\0tag"])
        assert to_native(produced.data) == {"integer": (1 << 63) - 1, "bytes": b"copy"}
        assert list(produced.tags) == ["nul\0tag"]
        with pool.fetch_view(since=sent.seq) as stream:
            forwarded = pool.feed_view(produced)
            assert to_native(forwarded.data) == to_native(produced.data)
            assert list(forwarded.tags) == list(produced.tags)
            sliced = pool.feed_view({"array": child[::-1], "bytes": produced.data["bytes"][::2]})
            assert to_native(sliced.data) == {"array": [3, 2, 1], "bytes": b"cp"}
            first = next(stream)
            batch = stream.next_batch(4, 1024 * 1024)
            assert first.seq == sent.seq and [item.seq for item in batch] == [produced.seq]
            try:
                next(stream)
            except StopIteration:
                pass
            else:
                raise AssertionError("finite view reader exceeded its opening boundary")

        del view
    gc.collect()
    assert list(child) == [1, 2, 3] and len(buffer) > 0
print("python-wheel-native-roundtrip-and-view-ok")
PY
  plasmite --version >/dev/null
  pls --version >/dev/null
  if [[ "$variant" == abi3 ]]; then
    BUNDLED_CLI="$(python -c 'from plasmite._cli import _bundled_cli_path; print(_bundled_cli_path())')"
    python "$ROOT/scripts/packaged_serve_smoke.py" --cli "$BUNDLED_CLI" --scratch "$WORKDIR"
  fi
  deactivate
done

echo "[smoke] Python abi3, cp310 and fallback wheel install + runtime ok"
