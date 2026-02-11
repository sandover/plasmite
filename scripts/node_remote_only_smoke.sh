#!/usr/bin/env bash
# Purpose: Validate remote-only behavior when npm tarball has no native payload.
# Exports: N/A (script entry point).
# Role: Ensures RemoteClient remains usable without native/{platform} assets.
# Invariants: Packages with native dirs removed and skips prepack scripts.
# Invariants: Client constructor and CLI must fail with a helpful native message.
# Notes: Uses workspace-local temp dirs under .scratch/.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODE_DIR="$ROOT/bindings/node"
mkdir -p "$ROOT/.scratch"
WORKDIR="$(mktemp -d "$ROOT/.scratch/node-remote-only-smoke.XXXXXX")"

backup_dir="$WORKDIR/native-backup"
if [[ -d "$NODE_DIR/native" ]]; then
  cp -R "$NODE_DIR/native" "$backup_dir"
fi

restore_native() {
  rm -rf "$NODE_DIR/native"
  if [[ -d "$backup_dir" ]]; then
    cp -R "$backup_dir" "$NODE_DIR/native"
  fi
}
trap restore_native EXIT

rm -rf "$NODE_DIR/native/linux-x64" "$NODE_DIR/native/darwin-x64" "$NODE_DIR/native/darwin-arm64"

(
  cd "$NODE_DIR"
  npm pack --ignore-scripts >/dev/null
)

TARBALL="$(cd "$NODE_DIR" && ls -1 plasmite-*.tgz | tail -n 1)"
if [[ -z "$TARBALL" ]]; then
  echo "failed to produce npm tarball for remote-only smoke"
  exit 1
fi

mkdir -p "$WORKDIR/app"
(
  cd "$WORKDIR/app"
  npm init -y >/dev/null
  npm install "$NODE_DIR/$TARBALL" >/dev/null
  node -e 'const { RemoteClient } = require("plasmite"); const r = new RemoteClient("http://127.0.0.1:9700"); if (!r) { throw new Error("missing remote client"); }'
  node -e 'const p = require("plasmite"); let ok = false; try { new p.Client("./x"); } catch (e) { ok = /native addon is unavailable|unsupported platform/i.test(String(e.message)); } if (!ok) { process.exit(1); }'
  set +e
  out="$(npx plasmite --version 2>&1)"
  code=$?
  set -e
  if [[ "$code" -eq 0 ]]; then
    echo "expected npx plasmite --version to fail without native payload"
    exit 1
  fi
  if ! grep -Eiq 'native CLI is unavailable|unsupported platform' <<<"$out"; then
    echo "unexpected CLI error output: $out"
    exit 1
  fi
)

rm -f "$NODE_DIR/$TARBALL"
echo "[smoke] node remote-only package behavior ok"
