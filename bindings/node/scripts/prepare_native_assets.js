/*
Purpose: Stage native SDK assets under native/{platform}/ before npm packing.
Key Exports: Script entrypoint only.
Role: Copy addon + shared library + CLI into platform-specific package subdir.
Invariants: Prefers PLASMITE_SDK_DIR (SDK layout), then repo-local target/debug.
Invariants: Fails when platform mapping or required artifacts are unavailable.
Notes: Intended for prepack usage and local packaging workflows.
*/

const fs = require("node:fs");
const path = require("node:path");

const packageRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(packageRoot, "..", "..");
const sdkRoot = process.env.PLASMITE_SDK_DIR
  ? path.resolve(process.env.PLASMITE_SDK_DIR)
  : path.join(repoRoot, "target", "debug");
const platformByOsArch = {
  "linux-x64": "linux-x64",
  "darwin-x64": "darwin-x64",
  "darwin-arm64": "darwin-arm64",
};
const runtimeKey = `${process.platform}-${process.arch}`;
const platformDir = platformByOsArch[runtimeKey];

if (!platformDir) {
  throw new Error(`Unsupported platform for native packaging: ${runtimeKey}`);
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
const addonCandidates = [path.join(packageRoot, "index.node")];

function firstExisting(candidates) {
  return candidates.find((candidate) => fs.existsSync(candidate));
}

const libSource = firstExisting(libCandidates);
if (!libSource) {
  throw new Error(`libplasmite not found in SDK root: ${sdkRoot}`);
}
const cliSource = firstExisting(cliCandidates);
if (!cliSource) {
  throw new Error(`plasmite CLI not found in SDK root: ${sdkRoot}`);
}
const addonSource = firstExisting(addonCandidates);
if (!addonSource) {
  throw new Error(`index.node not found in package root: ${packageRoot}`);
}

const destination = path.join(packageRoot, "native", platformDir);
if (fs.existsSync(destination)) {
  fs.rmSync(destination, { recursive: true, force: true });
}
fs.mkdirSync(destination, { recursive: true });

fs.copyFileSync(addonSource, path.join(destination, "index.node"));
fs.copyFileSync(libSource, path.join(destination, path.basename(libSource)));
const cliDest = path.join(destination, "plasmite");
fs.copyFileSync(cliSource, cliDest);
fs.chmodSync(cliDest, 0o755);
