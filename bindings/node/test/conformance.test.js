/*
Purpose: Run Node conformance manifests as part of tests.
Key Exports: None (node:test entry).
Role: Ensure Node binding conforms to the manifest suite.
Invariants: Uses local libplasmite and plasmite CLI binaries.
Notes: Requires PLASMITE_LIB_DIR and PLASMITE_BIN to be resolvable.
*/

const test = require("node:test");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

const repoRoot = path.resolve(__dirname, "..", "..", "..");
const binPath = process.env.PLASMITE_BIN || path.join(repoRoot, "target", "debug", "plasmite");
const libDir = process.env.PLASMITE_LIB_DIR || path.join(repoRoot, "target", "debug");

function runManifest(name) {
  const manifest = path.join(repoRoot, "conformance", name);
  const env = { ...process.env, PLASMITE_BIN: binPath };
  if (process.platform === "darwin") {
    env.DYLD_LIBRARY_PATH = env.DYLD_LIBRARY_PATH
      ? `${libDir}:${env.DYLD_LIBRARY_PATH}`
      : libDir;
  } else if (process.platform !== "win32") {
    env.LD_LIBRARY_PATH = env.LD_LIBRARY_PATH
      ? `${libDir}:${env.LD_LIBRARY_PATH}`
      : libDir;
  }

  execFileSync(process.execPath, [path.join(__dirname, "..", "cmd", "plasmite-conformance.js"), manifest], {
    stdio: "inherit",
    env,
  });
}

test("conformance sample", () => runManifest("sample-v0.json"));
test("conformance negative", () => runManifest("negative-v0.json"));
test("conformance multiprocess", () => runManifest("multiprocess-v0.json"));
test("conformance pool admin", () => runManifest("pool-admin-v0.json"));
