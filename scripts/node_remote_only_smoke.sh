#!/usr/bin/env bash
# Purpose: Validate remote-only behavior when npm tarball has no native payload.
# Exports: N/A (script entry point).
# Role: Ensures RemoteClient remains usable without native/{platform} assets.
# Invariants: Packages from a staging directory without native assets.
# Invariants: Client constructor and CLI must fail with a helpful native message.
# Notes: Uses workspace-local temp dirs under .scratch/.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODE_DIR="$ROOT/bindings/node"
mkdir -p "$ROOT/.scratch"
WORKDIR="$(mktemp -d "$ROOT/.scratch/node-remote-only-smoke.XXXXXX")"

cleanup() {
  local status=$?
  if [[ "$status" -eq 0 ]]; then
    rm -rf "$WORKDIR"
  else
    echo "[smoke] retained diagnostics: $WORKDIR" >&2
  fi
  return "$status"
}
trap cleanup EXIT

(
  cd "$NODE_DIR"
  npm pack --ignore-scripts --dry-run --json > "$WORKDIR/manifest.json"
)
stage="$WORKDIR/package"
node - "$NODE_DIR" "$stage" "$WORKDIR/manifest.json" <<'JS'
const { copyFileSync, mkdirSync, readFileSync } = require('node:fs');
const { dirname, join } = require('node:path');
const [source, stage, manifest] = process.argv.slice(2);
for (const entry of JSON.parse(readFileSync(manifest, 'utf8'))[0].files) {
  if (entry.path.startsWith('native/')) continue;
  const target = join(stage, entry.path);
  mkdirSync(dirname(target), { recursive: true });
  copyFileSync(join(source, entry.path), target);
}
JS
(
  cd "$stage"
  npm pack --ignore-scripts >/dev/null
)

TARBALL="$(cd "$stage" && ls -1 plasmite-*.tgz | tail -n 1)"
if [[ -z "$TARBALL" ]]; then
  echo "failed to produce npm tarball for remote-only smoke"
  exit 1
fi

mkdir -p "$WORKDIR/app"
(
  cd "$WORKDIR/app"
  npm init -y >/dev/null
  npm install "$stage/$TARBALL" >/dev/null
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

echo "[smoke] node remote-only package behavior ok"
