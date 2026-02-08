#!/usr/bin/env node
/*
Purpose: Provide an npm bin entrypoint that runs the bundled plasmite CLI.
Key Exports: CLI entrypoint only.
Role: Ensure `npx plasmite` works from npm-installed package artifacts.
Invariants: Uses packaged binary first, then falls back to PATH.
Invariants: Forwards argv verbatim and propagates child exit status.
Notes: Prints a concise error when no CLI binary is available.
*/

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const packageRoot = path.resolve(__dirname, "..");
const bundled = path.join(packageRoot, "plasmite");
const target = fs.existsSync(bundled) ? bundled : "plasmite";

const result = spawnSync(target, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(`plasmite: failed to execute ${target}: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
