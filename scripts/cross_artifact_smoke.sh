#!/usr/bin/env bash
# Purpose: Verify pool-format compatibility across CLI and language bindings.
# Key exports: N/A (script entry point).
# Role: Guardrail smoke test that catches stale artifact/version mismatch early.
# Invariants: Uses only workspace-local temp state under .scratch/.
# Invariants: Asserts all created pools carry the current format version.
# Notes: Default mode checks local debug artifacts; tarball mode checks release SDK layout.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIB_DIR="$ROOT/target/debug"
mkdir -p "$ROOT/.scratch"
WORKDIR="$(mktemp -d "$ROOT/.scratch/cross-artifact.XXXXXX")"
trap 'rm -rf "$WORKDIR"' EXIT
HAS_RG=0
if command -v rg >/dev/null 2>&1; then
  HAS_RG=1
fi

stream_matches() {
  local pattern="$1"
  if [[ "$HAS_RG" -eq 1 ]]; then
    rg -q "$pattern"
  else
    grep -Eq "$pattern"
  fi
}

require_file() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    echo "required file missing: $path"
    exit 1
  fi
}

smoke_release_tarball() {
  local tarball="$1"
  local target="${2:-}"
  local extract_dir="$WORKDIR/release-sdk"
  mkdir -p "$extract_dir"
  tar -C "$extract_dir" -xzf "$tarball"

  require_file "$extract_dir/bin/plasmite"
  require_file "$extract_dir/bin/pls"
  require_file "$extract_dir/include/plasmite.h"
  require_file "$extract_dir/lib/pkgconfig/plasmite.pc"

  local shared_lib=""
  if [[ -f "$extract_dir/lib/libplasmite.dylib" ]]; then
    shared_lib="$extract_dir/lib/libplasmite.dylib"
    local dylib_id
    dylib_id="$(otool -D "$shared_lib" | sed -n '2p' | xargs)"
    if [[ "$dylib_id" != "@rpath/libplasmite.dylib" ]]; then
      echo "unexpected dylib install_name: $dylib_id"
      exit 1
    fi
  elif [[ -f "$extract_dir/lib/libplasmite.so" ]]; then
    shared_lib="$extract_dir/lib/libplasmite.so"
    if command -v readelf >/dev/null 2>&1; then
      if ! readelf -d "$shared_lib" | stream_matches 'SONAME.*\[libplasmite\.so\]'; then
        echo "missing or unstable SONAME on $shared_lib"
        exit 1
      fi
    fi
  else
    echo "release SDK missing shared library"
    exit 1
  fi

  if command -v pkg-config >/dev/null 2>&1; then
    PKG_CONFIG_PATH="$extract_dir/lib/pkgconfig" pkg-config --modversion plasmite >/dev/null
    PKG_CONFIG_PATH="$extract_dir/lib/pkgconfig" pkg-config --cflags --libs plasmite >/dev/null
  else
    # Keep local smoke usable on minimal environments that lack pkg-config.
    if [[ "$HAS_RG" -eq 1 ]]; then
      rg -q '^Name: plasmite$' "$extract_dir/lib/pkgconfig/plasmite.pc"
      rg -q '^Libs: .* -lplasmite$' "$extract_dir/lib/pkgconfig/plasmite.pc"
      rg -q '^Cflags: (-I|.* -I)' "$extract_dir/lib/pkgconfig/plasmite.pc"
    else
      grep -Eq '^Name: plasmite$' "$extract_dir/lib/pkgconfig/plasmite.pc"
      grep -Eq '^Libs: .* -lplasmite$' "$extract_dir/lib/pkgconfig/plasmite.pc"
      grep -Eq '^Cflags: (-I|.* -I)' "$extract_dir/lib/pkgconfig/plasmite.pc"
    fi
  fi
  if [[ "$target" == "armv7-unknown-linux-gnueabihf" ]]; then
    local machine library_machine
    machine="$(readelf -h "$extract_dir/bin/plasmite" | sed -n 's/.*Machine:[[:space:]]*//p')"
    library_machine="$(readelf -h "$extract_dir/lib/libplasmite.so" | sed -n 's/.*Machine:[[:space:]]*//p')"
    [[ "$machine" == ARM && "$library_machine" == ARM ]] || {
      echo "expected ARMv7 CLI and library, got: $machine and $library_machine" >&2
      exit 1
    }
    for artifact in "$extract_dir/bin/plasmite" "$extract_dir/bin/pls" "$shared_lib"; do
      readelf -h "$artifact" | stream_matches 'Class:.*ELF32'
      readelf -h "$artifact" | stream_matches 'Flags:.*hard-float ABI'
    done
    readelf -l "$extract_dir/bin/plasmite" | stream_matches 'interpreter: /lib/ld-linux-armhf.so.3'
    qemu-arm -L /usr/arm-linux-gnueabihf -E "LD_LIBRARY_PATH=$extract_dir/lib" "$extract_dir/bin/plasmite" --version >/dev/null
    qemu-arm -L /usr/arm-linux-gnueabihf -E "LD_LIBRARY_PATH=$extract_dir/lib" "$extract_dir/bin/pls" --help >/dev/null
  else
    if [[ "$target" == "aarch64-unknown-linux-gnu" ]]; then
      for artifact in "$extract_dir/bin/plasmite" "$extract_dir/bin/pls" "$shared_lib"; do
        readelf -h "$artifact" | stream_matches 'Class:.*ELF64'
        readelf -h "$artifact" | stream_matches 'Machine:.*AArch64'
      done
    fi
    "$extract_dir/bin/plasmite" --version >/dev/null
    "$extract_dir/bin/pls" --help >/dev/null
  fi

  if [[ "$target" == "armv7-unknown-linux-gnueabihf" || "$target" == "aarch64-unknown-linux-gnu" ]]; then
    local compiler
    compiler=cc
    if [[ "$target" == "armv7-unknown-linux-gnueabihf" ]]; then
      compiler=arm-linux-gnueabihf-gcc
    fi
    cat > "$WORKDIR/sdk-smoke.c" <<'C'
#include "plasmite.h"

int main(int argc, char **argv) {
    plsm_client_t *client = 0;
    plsm_error_t *error = 0;
    if (argc != 2 || plsm_client_new(argv[1], &client, &error) != 0) {
        plsm_error_free(error);
        return 1;
    }
    plsm_client_free(client);
    return 0;
}
C
    local compiler_flags=()
    if [[ "$target" == "armv7-unknown-linux-gnueabihf" ]]; then
      compiler_flags=(-march=armv7-a -mfpu=vfpv3-d16 -mfloat-abi=hard)
    fi
    "$compiler" "${compiler_flags[@]}" -I "$extract_dir/include" "$WORKDIR/sdk-smoke.c" \
      -L "$extract_dir/lib" -lplasmite -o "$WORKDIR/sdk-smoke"
    if [[ "$target" == "armv7-unknown-linux-gnueabihf" ]]; then
      qemu-arm -L /usr/arm-linux-gnueabihf -E "LD_LIBRARY_PATH=$extract_dir/lib" "$WORKDIR/sdk-smoke" "$WORKDIR"
    else
      LD_LIBRARY_PATH="$extract_dir/lib" "$WORKDIR/sdk-smoke" "$WORKDIR"
    fi
    python3 "$ROOT/scripts/packaged_serve_smoke.py" \
      --cli "$extract_dir/bin/plasmite" --scratch "$WORKDIR/serve-smoke"
  fi
  echo "[smoke] release tarball SDK layout ok: $tarball"
}

if [[ $# -gt 0 ]]; then
  smoke_release_tarball "$1" "${2:-}"
  exit 0
fi

if [[ "${RUNNER_OS:-}" == "macOS" || "$(uname -s)" == "Darwin" ]]; then
  export DYLD_LIBRARY_PATH="$LIB_DIR${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
else
  export LD_LIBRARY_PATH="$LIB_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
export LIBRARY_PATH="$LIB_DIR${LIBRARY_PATH:+:$LIBRARY_PATH}"
export PLASMITE_LIB_DIR="$LIB_DIR"
export PLASMITE_BIN="$LIB_DIR/plasmite"
export GOCACHE="${GOCACHE:-$ROOT/.scratch/go-build}"
mkdir -p "$GOCACHE"

expected_version="$(
  sed -n 's/^pub const POOL_FORMAT_VERSION: u32 = \([0-9][0-9]*\);/\1/p' "$ROOT/src/core/format.rs"
)"
if [[ -z "$expected_version" ]]; then
  echo "failed to detect POOL_FORMAT_VERSION from src/core/format.rs"
  exit 1
fi

check_pool_version() {
  local pool_path="$1"
  local actual
  actual="$(od -An -j4 -N4 -tu4 "$pool_path" | tr -d '[:space:]')"
  if [[ "$actual" != "$expected_version" ]]; then
    echo "expected format version $expected_version, got $actual ($pool_path)"
    exit 1
  fi
}

echo "[smoke] Go -> Node, CLI -> open Node stream, Node -> Python -> CLI"
POOL_DIR="$WORKDIR/pools"
(
  cd "$ROOT/bindings/go"
  PKG_CONFIG=/usr/bin/true \
    CGO_CFLAGS="-I$ROOT/include" \
    CGO_LDFLAGS="-L$LIB_DIR" \
    go run ./cmd/cookbook-smoke-fixture "$POOL_DIR" >/dev/null
)
check_pool_version "$POOL_DIR/cookbook-smoke.plasmite"

(
  cd "$ROOT"
  node - "$POOL_DIR" <<'JS'
const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const { Client, parseMessage } = require("./bindings/node");
const poolDir = process.argv[2];
const client = new Client(poolDir);
const pool = client.openPool("cookbook-smoke");
try {
  const seed = pool.get(1n);
  assert.deepEqual(seed.data, { task: "resize", id: 1 });
  assert.deepEqual(seed.tags, ["cookbook"]);
  const stream = pool.openStream(2n, 1n, 5000n);
  try {
    execFileSync(process.env.PLASMITE_BIN, [
      "--dir", poolDir, "feed", "cookbook-smoke", '{"writer":"cli"}', "--tag", "external",
    ], { timeout: 10000 });
    const message = parseMessage(stream.nextJson());
    assert.equal(message.seq, 2n);
    assert.deepEqual(message.data, { writer: "cli" });
    assert.deepEqual(message.tags, ["external"]);
  } finally {
    stream.close();
  }
  assert.equal(pool.append({ writer: "node" }, ["handoff"]).seq, 3n);
} finally {
  pool.close();
  client.close();
}
JS
)

PYTHONPATH="$ROOT/bindings/python" python3 - "$POOL_DIR" <<'PYTHON'
import json
import os
import subprocess
import sys
from plasmite import Client

pool_dir = sys.argv[1]
with Client(pool_dir) as client, client.open_pool("cookbook-smoke") as pool:
    message = pool.get(3)
    assert message.data == {"writer": "node"}
    assert message.meta.tags == ["handoff"]
    assert pool.append({"writer": "python"}, ["handoff"]).seq == 4

out = subprocess.check_output(
    [os.environ["PLASMITE_BIN"], "--dir", pool_dir, "fetch", "cookbook-smoke", "4", "--format", "json"],
    timeout=10,
)
message = json.loads(out)
assert message["seq"] == 4
assert message["data"] == {"writer": "python"}
assert message["meta"]["tags"] == ["handoff"]
PYTHON

echo "[smoke] complete"
