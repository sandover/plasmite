/*
Purpose: Stage native SDK assets in the npm package root before packing.
Key Exports: Script entrypoint only.
Role: Copy libplasmite + plasmite CLI beside index.node for install-time use.
Invariants: Prefers PLASMITE_SDK_DIR (SDK layout), then repo-local target/debug.
Invariants: Removes stale bundled artifacts before copying new ones.
Notes: Intended for prepack usage; fails fast if required assets are missing.
*/

const fs = require("node:fs");
const path = require("node:path");

const packageRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(packageRoot, "..", "..");
const sdkRoot = process.env.PLASMITE_SDK_DIR
  ? path.resolve(process.env.PLASMITE_SDK_DIR)
  : path.join(repoRoot, "target", "debug");

const staleArtifacts = [
  "libplasmite.dylib",
  "libplasmite.so",
  "libplasmite.a",
  "plasmite",
];

for (const artifact of staleArtifacts) {
  const candidate = path.join(packageRoot, artifact);
  if (fs.existsSync(candidate)) {
    fs.rmSync(candidate, { force: true });
  }
}

const libCandidates = [
  path.join(sdkRoot, "lib", "libplasmite.dylib"),
  path.join(sdkRoot, "lib", "libplasmite.so"),
  path.join(sdkRoot, "libplasmite.dylib"),
  path.join(sdkRoot, "libplasmite.so"),
];
const cliCandidates = [
  path.join(sdkRoot, "bin", "plasmite"),
  path.join(sdkRoot, "plasmite"),
];

function firstExisting(candidates) {
  return candidates.find((candidate) => fs.existsSync(candidate));
}

const libSource = firstExisting(libCandidates);
if (!libSource) {
  throw new Error(`libplasmite not found in SDK root: ${sdkRoot}`);
}
fs.copyFileSync(libSource, path.join(packageRoot, path.basename(libSource)));

const cliSource = firstExisting(cliCandidates);
if (!cliSource) {
  throw new Error(`plasmite CLI not found in SDK root: ${sdkRoot}`);
}
const cliDest = path.join(packageRoot, "plasmite");
fs.copyFileSync(cliSource, cliDest);
fs.chmodSync(cliDest, 0o755);
