/*
Purpose: Validate runtime exports and TypeScript value declarations stay aligned.
Key Exports: Script entrypoint only.
Role: CI/test drift gate for Node binding declaration/runtime consistency.
Invariants: Compares runtime `module.exports` keys with `types.d.ts` value exports.
Invariants: Ignores type-only declarations (interfaces/types).
Notes: Runs as part of `npm test`.
*/

const fs = require("node:fs");
const path = require("node:path");

const packageRoot = path.resolve(__dirname, "..");
const runtimeExports = Object.keys(require(path.join(packageRoot, "index.js"))).sort();
const declarations = fs.readFileSync(path.join(packageRoot, "types.d.ts"), "utf8");

const declaredValueExports = declarations
  .split("\n")
  .map((line) => line.trim())
  .flatMap((line) => {
    let match = line.match(/^export const enum (\w+)/);
    if (match) {
      return [match[1]];
    }
    match = line.match(/^export (const|class|function) (\w+)/);
    if (match) {
      return [match[2]];
    }
    return [];
  })
  .sort();

const declaredSet = new Set(declaredValueExports);
const runtimeSet = new Set(runtimeExports);

const missingInTypes = runtimeExports.filter((name) => !declaredSet.has(name));
const missingInRuntime = declaredValueExports.filter((name) => !runtimeSet.has(name));

if (missingInTypes.length || missingInRuntime.length) {
  if (missingInTypes.length) {
    console.error(`Missing in types.d.ts: ${missingInTypes.join(", ")}`);
  }
  if (missingInRuntime.length) {
    console.error(`Missing in runtime exports: ${missingInRuntime.join(", ")}`);
  }
  process.exit(1);
}
