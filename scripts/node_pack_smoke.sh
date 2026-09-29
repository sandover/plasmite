#!/usr/bin/env bash
# Purpose: Validate multi-platform npm tarball install and runtime behavior.
# Exports: N/A (script entry point).
# Role: Local/CI smoke gate for native/{platform} layout and CLI entrypoint.
# Invariants: Uses workspace-local temp dirs under .scratch/.
# Invariants: Requires npm, Node.js, Python 3, and a built SDK (target/debug by default).
# Notes: Set PLASMITE_SDK_DIR to test alternate SDK layouts.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODE_DIR="$ROOT/bindings/node"
mkdir -p "$ROOT/.scratch"
WORKDIR="$(mktemp -d "$ROOT/.scratch/node-pack-smoke.XXXXXX")"
cleanup_workdir() {
  local exit_status=$?
  if [[ "$exit_status" -eq 0 ]]; then
    rm -rf "$WORKDIR"
  fi
}
trap cleanup_workdir EXIT
source "$ROOT/scripts/normalize_sdk_layout.sh"
SDK_INPUT_DIR="${PLASMITE_SDK_DIR:-$ROOT/target/debug}"
SDK_DIR="$(plasmite_normalize_sdk_dir "$SDK_INPUT_DIR" "$WORKDIR/sdk" "Node smoke SDK")"
platform_key() {
  local os arch
  os="$(node -p 'process.platform')"
  arch="$(node -p 'process.arch')"
  case "${os}-${arch}" in
    linux-x64) echo "linux-x64" ;;
    linux-arm64) echo "linux-arm64" ;;
    darwin-x64) echo "darwin-x64" ;;
    darwin-arm64) echo "darwin-arm64" ;;
    win32-x64) echo "win32-x64" ;;
    *)
      echo "unsupported platform for node_pack_smoke: ${os}-${arch}" >&2
      return 1
      ;;
  esac
}
CURRENT_PLATFORM="$(platform_key)"
HAS_RG=0
if command -v rg >/dev/null 2>&1; then
  HAS_RG=1
fi

archive_has_member() {
  local archive="$1"
  local pattern="$2"
  local members
  members="$(tar -tzf "$archive")"
  if [[ "$HAS_RG" -eq 1 ]]; then
    rg -q "$pattern" <<<"$members"
  else
    grep -Eq "$pattern" <<<"$members"
  fi
}

(
  cd "$NODE_DIR"
  if [[ ! -f "$NODE_DIR/index.node" ]]; then
    PLASMITE_LIB_DIR="$SDK_DIR/lib" npm run build >/dev/null
  fi
  current_target="$(jq -r --arg platform "$CURRENT_PLATFORM" '
    [.targets[] | select(.node_platform == $platform)][0].rust_target // empty
  ' "$ROOT/release/targets.json")"
  if [[ -z "$current_target" ]]; then
    echo "failed to resolve Rust target for Node platform: $CURRENT_PLATFORM" >&2
    exit 1
  fi
  "$ROOT/scripts/package_node_natives.sh" "$current_target" "$SDK_DIR" "$NODE_DIR/index.node"
  PLASMITE_SDK_DIR="$SDK_DIR" PLASMITE_LIB_DIR="$SDK_DIR/lib" npm pack >/dev/null
)

TARBALL="$(cd "$NODE_DIR" && ls -1 plasmite-*.tgz | tail -n 1)"
if [[ -z "$TARBALL" ]]; then
  echo "failed to produce npm tarball"
  exit 1
fi

archive_has_member "$NODE_DIR/$TARBALL" "package/native/${CURRENT_PLATFORM}/index\\.node"
if [[ "$CURRENT_PLATFORM" == "win32-x64" ]]; then
  archive_has_member "$NODE_DIR/$TARBALL" "package/native/${CURRENT_PLATFORM}/plasmite\\.dll"
  archive_has_member "$NODE_DIR/$TARBALL" "package/native/${CURRENT_PLATFORM}/plasmite\\.exe"
else
  archive_has_member "$NODE_DIR/$TARBALL" "package/native/${CURRENT_PLATFORM}/libplasmite\\.(dylib|so)"
  archive_has_member "$NODE_DIR/$TARBALL" "package/native/${CURRENT_PLATFORM}/plasmite"
fi
archive_has_member "$NODE_DIR/$TARBALL" 'package/bin/plasmite\.js'

mkdir -p "$WORKDIR/app"
(
  cd "$WORKDIR/app"
  npm init -y >/dev/null
  npm install "$NODE_DIR/$TARBALL" >/dev/null
  node -e 'const { Client, Durability, RemoteClient } = require("plasmite"); const os = require("node:os"); const fs = require("node:fs"); const path = require("node:path"); const dir = fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-node-pack-")); let c; try { c = new Client(dir); const p = c.createPool("smoke", 1024*1024); p.appendJson(Buffer.from("{\"kind\":\"smoke\"}"), ["smoke"], Durability.Fast); const got = p.getJson(1); if (!got || got.length === 0) { throw new Error("empty get result"); } p.close(); c.close(); c = undefined; const rc = new RemoteClient("http://127.0.0.1:9700"); if (!rc) { throw new Error("missing RemoteClient"); } } finally { try { if (c) c.close(); } finally { fs.rmSync(dir, { recursive: true, force: true }); } }'
  npx plasmite --version >/dev/null
  cli_binary="$WORKDIR/app/node_modules/plasmite/native/$CURRENT_PLATFORM/plasmite"
  if [[ "$CURRENT_PLATFORM" == "win32-x64" ]]; then
    cli_binary+=".exe"
  fi
  smoke_python="$(command -v python || command -v python3 || true)"
  if [[ -z "$smoke_python" ]]; then
    echo "error: Python 3 is required for the packaged serving smoke." >&2
    exit 2
  fi
  "$smoke_python" "$ROOT/scripts/packaged_serve_smoke.py" --cli "$cli_binary" --scratch "$WORKDIR"
)

rm -f "$NODE_DIR/$TARBALL"
echo "[smoke] node pack install + runtime ok"
