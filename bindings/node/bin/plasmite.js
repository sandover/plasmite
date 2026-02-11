#!/usr/bin/env node
/*
Purpose: Provide an npm bin entrypoint that runs the packaged platform CLI binary.
Key Exports: CLI entrypoint only.
Role: Ensure `npx plasmite` works from npm-installed package artifacts.
Invariants: Resolves CLI from native/{platform}/ for supported platforms.
Invariants: Forwards argv verbatim and propagates child exit status.
Notes: Prints a concise error when no CLI binary is available.
*/

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const PLATFORM_DIRS = Object.freeze({
  linux: Object.freeze({ x64: "linux-x64", arm64: "linux-arm64" }),
  darwin: Object.freeze({ x64: "darwin-x64", arm64: "darwin-arm64" }),
});

function resolvePlatformDir() {
  const byArch = PLATFORM_DIRS[process.platform];
  if (!byArch) {
    return null;
  }
  return byArch[process.arch] ?? null;
}

const packageRoot = path.resolve(__dirname, "..");
const platformDir = resolvePlatformDir();
const target = platformDir
  ? path.join(packageRoot, "native", platformDir, "plasmite")
  : null;

if (!target || !fs.existsSync(target)) {
  console.error(
    `plasmite: native CLI is unavailable for ${process.platform}-${process.arch}. ` +
      "This package supports remote-only mode without native binaries on unsupported platforms.",
  );
  process.exit(1);
}

const result = spawnSync(target, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(`plasmite: failed to execute ${target}: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
