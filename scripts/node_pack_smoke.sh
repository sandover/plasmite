#!/usr/bin/env bash
# Purpose: Validate npm package tarball install and runtime behavior for Node bindings.
# Exports: N/A (script entry point).
# Role: Local/CI smoke gate for bundled native assets and CLI entrypoint.
# Invariants: Uses workspace-local temp dirs under .scratch/.
# Invariants: Requires npm + node and a built local SDK under target/debug by default.
# Notes: Set PLASMITE_SDK_DIR to test alternate SDK layouts.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODE_DIR="$ROOT/bindings/node"
mkdir -p "$ROOT/.scratch"
WORKDIR="$(mktemp -d "$ROOT/.scratch/node-pack-smoke.XXXXXX")"
SDK_DIR="${PLASMITE_SDK_DIR:-$ROOT/target/debug}"

(
  cd "$NODE_DIR"
  PLASMITE_SDK_DIR="$SDK_DIR" npm pack >/dev/null
)

TARBALL="$(cd "$NODE_DIR" && ls -1 plasmite-*.tgz | tail -n 1)"
if [[ -z "$TARBALL" ]]; then
  echo "failed to produce npm tarball"
  exit 1
fi

tar -tzf "$NODE_DIR/$TARBALL" | rg -q 'package/index\.node'
tar -tzf "$NODE_DIR/$TARBALL" | rg -q 'package/libplasmite\.(dylib|so)'
tar -tzf "$NODE_DIR/$TARBALL" | rg -q 'package/plasmite'
tar -tzf "$NODE_DIR/$TARBALL" | rg -q 'package/bin/plasmite\.js'

mkdir -p "$WORKDIR/app"
(
  cd "$WORKDIR/app"
  npm init -y >/dev/null
  npm install "$NODE_DIR/$TARBALL" >/dev/null
  node -e 'const { Client, Durability } = require("plasmite"); const os = require("node:os"); const fs = require("node:fs"); const path = require("node:path"); const dir = fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-node-pack-")); const c = new Client(dir); const p = c.createPool("smoke", 1024*1024); p.appendJson(Buffer.from("{\"kind\":\"smoke\"}"), ["smoke"], Durability.Fast); const got = p.getJson(1); if (!got || got.length === 0) { throw new Error("empty get result"); } p.close(); c.close(); console.log(typeof Client);'
  npx plasmite --version >/dev/null
)

rm -f "$NODE_DIR/$TARBALL"
echo "[smoke] node pack install + runtime ok"
