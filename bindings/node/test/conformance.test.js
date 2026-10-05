/*
Purpose: Run Node conformance manifests as part of tests.
Key Exports: None (node:test entry).
Role: Ensure Node binding conforms to the manifest suite.
Invariants: Uses local libplasmite and plasmite CLI binaries.
Notes: Requires PLASMITE_LIB_DIR and PLASMITE_BIN to be resolvable.
*/

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { execFileSync, spawnSync } = require("node:child_process");

const repoRoot = path.resolve(__dirname, "..", "..", "..");
const binPath = process.env.PLASMITE_BIN || path.join(repoRoot, "target", "debug", "plasmite");
const libDir = process.env.PLASMITE_LIB_DIR || path.join(repoRoot, "target", "debug");

function runnerEnv(overrides = {}) {
  const env = { ...process.env, PLASMITE_BIN: binPath, ...overrides };
  if (process.platform === "darwin") {
    env.DYLD_LIBRARY_PATH = env.DYLD_LIBRARY_PATH
      ? `${libDir}:${env.DYLD_LIBRARY_PATH}`
      : libDir;
  } else if (process.platform !== "win32") {
    env.LD_LIBRARY_PATH = env.LD_LIBRARY_PATH
      ? `${libDir}:${env.LD_LIBRARY_PATH}`
      : libDir;
  }

  return env;
}

const runnerPath = path.join(__dirname, "..", "cmd", "plasmite-conformance.js");

function runManifest(name) {
  const manifest = path.join(repoRoot, "conformance", name);
  execFileSync(process.execPath, [runnerPath, manifest], {
    stdio: "inherit",
    env: runnerEnv(),
  });
}

test("conformance sample", () => runManifest("sample-v0.json"));
test("conformance negative", () => runManifest("negative-v0.json"));
test("conformance multiprocess", () => runManifest("multiprocess-v0.json"));
test("conformance pool admin", () => runManifest("pool-admin-v0.json"));
test("conformance retention gap", () => runManifest("retention-gap-v0.json"));

test("conformance runner rejects unsafe workdirs before clearing files", () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-conformance-"));
  try {
    const sentinel = path.join(tempDir, "keep.txt");
    const sibling = path.join(tempDir, "src");
    const manifestPath = path.join(tempDir, "manifest.json");
    fs.writeFileSync(sentinel, "keep");
    fs.mkdirSync(sibling);
    fs.writeFileSync(path.join(sibling, "keep.txt"), "keep");
    for (const workdir of ["", null, ".", "..", "src", "work-", "work.", "nested/work", "nested\\work", "work:drive", "work\0dir"]) {
      fs.writeFileSync(manifestPath, JSON.stringify({ conformance_version: 0, workdir, steps: [] }));
      const result = spawnSync(process.execPath, [runnerPath, manifestPath], {
        encoding: "utf8",
        env: runnerEnv(),
      });
      assert.notEqual(result.status, 0, `workdir ${JSON.stringify(workdir)} should fail`);
      assert.match(result.stderr, /workdir must be work or a work- name/);
      assert.equal(fs.readFileSync(sentinel, "utf8"), "keep");
      assert.equal(fs.readFileSync(path.join(sibling, "keep.txt"), "utf8"), "keep");
    }
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("conformance runner rejects a workdir symlink", { skip: process.platform === "win32" }, () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-conformance-"));
  try {
    const target = path.join(tempDir, "target");
    fs.mkdirSync(target);
    fs.writeFileSync(path.join(target, "keep.txt"), "keep");
    fs.symlinkSync(target, path.join(tempDir, "work"), "dir");
    const manifestPath = path.join(tempDir, "manifest.json");
    fs.writeFileSync(manifestPath, JSON.stringify({ conformance_version: 0, workdir: "work", steps: [] }));
    const result = spawnSync(process.execPath, [runnerPath, manifestPath], {
      encoding: "utf8",
      env: runnerEnv(),
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /workdir must not be a symbolic link/);
    assert.equal(fs.readFileSync(path.join(target, "keep.txt"), "utf8"), "keep");
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("conformance runner compares CLI error fields as data", { skip: process.platform === "win32" }, () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-conformance-"));
  try {
    const fakeBin = path.join(tempDir, "fake-plasmite");
    fs.writeFileSync(
      fakeBin,
      '#!/usr/bin/env node\nconsole.error(JSON.stringify({error:{kind:"NotFound",message:"before; after; marker",path:"/missing"}}));process.exit(1);\n',
      { mode: 0o755 },
    );
    const manifestPath = path.join(tempDir, "manifest.json");
    fs.writeFileSync(manifestPath, JSON.stringify({
      conformance_version: 0,
      steps: [{
        op: "pool_info",
        pool: "missing",
        expect: { error: { kind: "NotFound", message_contains: "after; marker", has_path: true } },
      }],
    }));
    const result = spawnSync(process.execPath, [runnerPath, manifestPath], {
      encoding: "utf8",
      env: runnerEnv({ PLASMITE_BIN: fakeBin }),
    });
    assert.equal(result.status, 0, result.stderr);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("conformance runner validates every writer before spawning", { skip: process.platform === "win32" }, async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-conformance-"));
  try {
    const marker = path.join(tempDir, "spawned.txt");
    const fakeBin = path.join(tempDir, "fake-plasmite");
    fs.writeFileSync(
      fakeBin,
      '#!/usr/bin/env node\nrequire("node:fs").writeFileSync(process.env.PLASMITE_MARKER,"spawned");\n',
      { mode: 0o755 },
    );
    const manifestPath = path.join(tempDir, "manifest.json");
    fs.writeFileSync(manifestPath, JSON.stringify({
      conformance_version: 0,
      steps: [{ op: "spawn_poke", pool: "test", input: { messages: [{ data: { valid: true } }, {}] } }],
    }));
    const result = spawnSync(process.execPath, [runnerPath, manifestPath], {
      encoding: "utf8",
      env: runnerEnv({ PLASMITE_BIN: fakeBin, PLASMITE_MARKER: marker }),
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /message.data is required/);
    await new Promise((resolve) => setTimeout(resolve, 500));
    assert.equal(fs.existsSync(marker), false);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("conformance runner waits for all started writers before failing", { skip: process.platform === "win32" }, () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-conformance-"));
  try {
    const marker = path.join(tempDir, "completed.txt");
    const fakeBin = path.join(tempDir, "fake-plasmite");
    fs.writeFileSync(
      fakeBin,
      '#!/usr/bin/env node\nconst fs=require("node:fs");if(process.argv.includes(\'{"fail":true}\'))process.exit(1);setTimeout(()=>fs.writeFileSync(process.env.PLASMITE_MARKER,"done"),500);\n',
      { mode: 0o755 },
    );
    const manifestPath = path.join(tempDir, "manifest.json");
    fs.writeFileSync(manifestPath, JSON.stringify({
      conformance_version: 0,
      steps: [{ op: "spawn_poke", pool: "test", input: { messages: [{ data: { fail: true } }, { data: { slow: true } }] } }],
    }));
    const result = spawnSync(process.execPath, [runnerPath, manifestPath], {
      encoding: "utf8",
      env: runnerEnv({ PLASMITE_BIN: fakeBin, PLASMITE_MARKER: marker }),
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /feed process failed/);
    assert.equal(fs.readFileSync(marker, "utf8"), "done");
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});
