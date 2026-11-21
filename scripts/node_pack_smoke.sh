#!/usr/bin/env bash
# Purpose: Validate multi-platform npm tarball install and runtime behavior.
# Exports: N/A (script entry point).
# Role: Local/CI smoke gate for native/{platform} layout and CLI entrypoint.
# Invariants: Uses workspace-local temp dirs under .scratch/.
# Invariants: Requires npm, Node.js, Python 3, and a built SDK (target/debug by default).
# Notes: Pass an existing tarball to test it directly, or set PLASMITE_SDK_DIR to build one from an alternate SDK.

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

if [[ $# -gt 1 ]]; then
  echo "usage: node_pack_smoke.sh [tarball]" >&2
  exit 2
fi
(cd "$NODE_DIR" && npm run check:portable >/dev/null)
if [[ $# -eq 1 ]]; then
  TARBALL="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
else
  source "$ROOT/scripts/normalize_sdk_layout.sh"
  SDK_INPUT_DIR="${PLASMITE_SDK_DIR:-$ROOT/target/debug}"
  SDK_DIR="$(plasmite_normalize_sdk_dir "$SDK_INPUT_DIR" "$WORKDIR/sdk" "Node smoke SDK")"
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
    npm pack --ignore-scripts --silent --pack-destination "$WORKDIR" > "$WORKDIR/tarball-name"
  )
  TARBALL="$WORKDIR/$(cat "$WORKDIR/tarball-name")"
fi

archive_has_member "$TARBALL" "package/native/${CURRENT_PLATFORM}/index\\.node"
if [[ "$CURRENT_PLATFORM" == "win32-x64" ]]; then
  archive_has_member "$TARBALL" "package/native/${CURRENT_PLATFORM}/plasmite2\\.dll"
  archive_has_member "$TARBALL" "package/native/${CURRENT_PLATFORM}/plasmite\\.exe"
else
  archive_has_member "$TARBALL" "package/native/${CURRENT_PLATFORM}/libplasmite\\.(2\\.dylib|so\\.2)"
  archive_has_member "$TARBALL" "package/native/${CURRENT_PLATFORM}/plasmite"
fi
archive_has_member "$TARBALL" 'package/bin/plasmite\.js'

mkdir -p "$WORKDIR/app"
(
  cd "$WORKDIR/app"
  npm init -y >/dev/null
  npm install "$TARBALL" >/dev/null
  node <<'JS'
const assert = require("node:assert/strict");
const { Client, Message, RemoteClient, toNative } = require("plasmite");
const { mkdtempSync, rmSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const dir = mkdtempSync(join(tmpdir(), "plasmite-node-pack-"));
(async () => {
  try {
    const client = new Client(dir);
    try {
      const pool = client.openPool("events", { create: true });
      try {
        const tags = ["smoke", "a\0b"];
        const reader = await pool.follow({ tags });
        try {
          const stored = await pool.feed({ event: "smoke" }, { tags });
          const next = await reader.next(2);
          assert.equal(next.value.seq, stored.seq);
          assert.equal(next.value.time, stored.time);
          assert.deepEqual(next.value.tags, tags);
          assert.equal(typeof stored.time, "bigint");
          assert.equal(pool.info().bounds.oldest, stored.seq);
          const fetch = await pool.fetch({ since: stored.seq });
          const fetched = (await fetch.next()).value;
          assert.ok(fetched instanceof Message);
          assert.deepEqual(toNative(fetched.data), { event: "smoke" });
          assert.deepEqual(fetched.raw, stored.raw);
          assert.equal((await fetch.next()).done, true);
        } finally {
          await reader.return();
        }
      } finally {
        pool.close();
      }
    } finally {
      client.close();
    }
    assert.ok(new RemoteClient("http://127.0.0.1:9700"));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
JS
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

echo "[smoke] node pack install + runtime ok"
