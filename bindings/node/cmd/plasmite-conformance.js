/*
Purpose: Execute conformance manifests against the Node binding.
Key Exports: None (script entry point).
Role: Reference runner for JSON conformance manifests in JS.
Invariants: Manifests are JSON-only; steps execute in order; fail-fast on errors.
Invariants: Workdir is isolated under the manifest directory.
Notes: Mirrors Rust/Go conformance runner behavior.
*/

const fs = require("node:fs");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");
const { Client, Durability } = require("../index.js");

async function main() {
  const args = process.argv.slice(2);
  if (args.length !== 1) {
    throw new Error("usage: plasmite-conformance <path/to/manifest.json>");
  }
  const manifestPath = args[0];
  const manifestDir = path.dirname(manifestPath);
  const repoRoot = path.dirname(manifestDir);

  const content = fs.readFileSync(manifestPath, "utf8");
  const manifest = JSON.parse(content);

  if (manifest.conformance_version !== 0) {
    throw new Error(`unsupported conformance_version: ${manifest.conformance_version}`);
  }

  const workdir = manifest.workdir || "work";
  const workdirPath = path.join(manifestDir, workdir);
  resetWorkdir(workdirPath);

  const client = new Client(workdirPath);

  for (let index = 0; index < manifest.steps.length; index += 1) {
    const step = manifest.steps[index];
    const stepId = step.id ?? null;
    const op = step.op;
    if (!op) {
      throw stepError(index, stepId, "missing op");
    }
    switch (op) {
      case "create_pool":
        runCreatePool(client, step, index, stepId);
        break;
      case "append":
        runAppend(client, step, index, stepId);
        break;
      case "get":
        runGet(client, step, index, stepId);
        break;
      case "tail":
        runTail(client, step, index, stepId);
        break;
      case "list_pools":
        runListPools(step, index, stepId, workdirPath);
        break;
      case "pool_info":
        runPoolInfo(repoRoot, workdirPath, step, index, stepId);
        break;
      case "delete_pool":
        runDeletePool(step, index, stepId, workdirPath);
        break;
      case "spawn_poke":
        await runSpawnPoke(repoRoot, workdirPath, step, index, stepId);
        break;
      case "corrupt_pool_header":
        runCorruptPoolHeader(workdirPath, step, index, stepId);
        break;
      case "chmod_path":
        runChmodPath(step, index, stepId);
        break;
      default:
        throw stepError(index, stepId, `unknown op: ${op}`);
    }
  }
}

function resetWorkdir(dir) {
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
}

function runCreatePool(client, step, index, stepId) {
  const pool = requirePool(step, index, stepId);
  const sizeBytes = step.input?.size_bytes ?? 1024 * 1024;
  const result = tryCall(() => client.createPool(pool, BigInt(sizeBytes)));
  if (result.value) {
    result.value.close();
  }
  validateExpectError(step.expect, result.error, index, stepId);
}

function runAppend(client, step, index, stepId) {
  const pool = requirePool(step, index, stepId);
  const input = requireInput(step, index, stepId);
  const payload = input.data;
  if (payload === undefined) {
    throw stepError(index, stepId, "missing input.data");
  }
  const descrips = input.descrips ?? [];

  const poolHandle = tryCall(() => client.openPool(pool));
  if (poolHandle.error) {
    validateExpectError(step.expect, poolHandle.error, index, stepId);
    return;
  }

  const result = tryCall(() =>
    poolHandle.value.appendJson(
      Buffer.from(JSON.stringify(payload)),
      descrips,
      Durability.Fast
    )
  );
  poolHandle.value.close();
  if (result.error) {
    validateExpectError(step.expect, result.error, index, stepId);
    return;
  }
  validateExpectError(step.expect, null, index, stepId);

  if (step.expect && typeof step.expect.seq === "number") {
    const message = JSON.parse(result.value.toString("utf8"));
    if (message.seq !== step.expect.seq) {
      throw stepError(index, stepId, `expected seq ${step.expect.seq}, got ${message.seq}`);
    }
  }
}

function runGet(client, step, index, stepId) {
  const pool = requirePool(step, index, stepId);
  const input = requireInput(step, index, stepId);
  if (typeof input.seq !== "number") {
    throw stepError(index, stepId, "missing input.seq");
  }

  const poolHandle = tryCall(() => client.openPool(pool));
  if (poolHandle.error) {
    validateExpectError(step.expect, poolHandle.error, index, stepId);
    return;
  }

  const result = tryCall(() => poolHandle.value.getJson(BigInt(input.seq)));
  poolHandle.value.close();
  if (result.error) {
    validateExpectError(step.expect, result.error, index, stepId);
    return;
  }
  validateExpectError(step.expect, null, index, stepId);

  const message = JSON.parse(result.value.toString("utf8"));
  if (step.expect?.data !== undefined && !deepEqual(step.expect.data, message.data)) {
    throw stepError(index, stepId, "data mismatch");
  }
  if (step.expect?.descrips !== undefined && !deepEqual(step.expect.descrips, message.meta.descrips)) {
    throw stepError(index, stepId, "descrips mismatch");
  }
}

function runTail(client, step, index, stepId) {
  const pool = requirePool(step, index, stepId);
  const input = step.input ?? {};
  const sinceSeq = typeof input.since_seq === "number" ? BigInt(input.since_seq) : undefined;
  const maxMessages = typeof input.max === "number" ? BigInt(input.max) : undefined;
  const expect = step.expect ?? null;

  const poolHandle = tryCall(() => client.openPool(pool));
  if (poolHandle.error) {
    validateExpectError(step.expect, poolHandle.error, index, stepId);
    return;
  }

  const streamResult = tryCall(() =>
    poolHandle.value.openStream(sinceSeq, maxMessages, BigInt(500))
  );
  if (streamResult.error) {
    poolHandle.value.close();
    validateExpectError(step.expect, streamResult.error, index, stepId);
    return;
  }

  const messages = [];
  while (true) {
    const next = streamResult.value.nextJson();
    if (!next) {
      break;
    }
    messages.push(JSON.parse(next.toString("utf8")));
    if (maxMessages && BigInt(messages.length) >= maxMessages) {
      break;
    }
  }
  streamResult.value.close();
  poolHandle.value.close();

  validateExpectError(step.expect, null, index, stepId);

  const expected = expectedMessages(expect, index, stepId);
  if (messages.length !== expected.messages.length) {
    throw stepError(index, stepId, `expected ${expected.messages.length} messages, got ${messages.length}`);
  }

  for (let i = 1; i < messages.length; i += 1) {
    if (messages[i - 1].seq >= messages[i].seq) {
      throw stepError(index, stepId, "tail messages out of order");
    }
  }

  if (expected.ordered) {
    expected.messages.forEach((entry, idx) => {
      if (!deepEqual(entry.data, messages[idx].data)) {
        throw stepError(index, stepId, "data mismatch");
      }
      if (entry.descrips && !deepEqual(entry.descrips, messages[idx].meta.descrips)) {
        throw stepError(index, stepId, "descrips mismatch");
      }
    });
  } else {
    const used = new Array(messages.length).fill(false);
    expected.messages.forEach((entry) => {
      let found = false;
      for (let idx = 0; idx < messages.length; idx += 1) {
        if (used[idx]) continue;
        if (!deepEqual(entry.data, messages[idx].data)) continue;
        if (entry.descrips && !deepEqual(entry.descrips, messages[idx].meta.descrips)) continue;
        used[idx] = true;
        found = true;
        break;
      }
      if (!found) {
        throw stepError(index, stepId, "message mismatch");
      }
    });
  }
}

function runListPools(step, index, stepId, workdirPath) {
  const result = tryCall(() => listPoolNames(workdirPath));
  if (result.error) {
    validateExpectError(step.expect, result.error, index, stepId);
    return;
  }
  validateExpectError(step.expect, null, index, stepId);
  if (step.expect?.names) {
    if (!Array.isArray(step.expect.names)) {
      throw stepError(index, stepId, "expect.names must be array");
    }
    const actual = [...result.value].sort();
    const expected = [...step.expect.names].sort();
    if (!deepEqual(actual, expected)) {
      throw stepError(
        index,
        stepId,
        `pool list mismatch: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`
      );
    }
  }
}

function runPoolInfo(repoRoot, workdirPath, step, index, stepId) {
  const pool = requirePool(step, index, stepId);
  const plasmiteBin = resolvePlasmiteBin(repoRoot);
  const result = spawnSync(
    plasmiteBin,
    ["--dir", workdirPath, "pool", "info", pool],
    { encoding: "utf8" }
  );
  if (result.status !== 0) {
    const err = parseErrorJSON(result.stderr);
    validateExpectError(step.expect, err, index, stepId);
    return;
  }
  validateExpectError(step.expect, null, index, stepId);
  const info = JSON.parse(result.stdout);
  if (step.expect?.file_size !== undefined && info.file_size !== step.expect.file_size) {
    throw stepError(index, stepId, "file_size mismatch");
  }
  if (step.expect?.ring_size !== undefined && info.ring_size !== step.expect.ring_size) {
    throw stepError(index, stepId, "ring_size mismatch");
  }
  if (step.expect?.bounds) {
    expectBounds(step.expect.bounds, info.bounds ?? {}, index, stepId);
  }
}

function runDeletePool(step, index, stepId, workdirPath) {
  const pool = requirePool(step, index, stepId);
  const poolPath = resolvePoolPath(workdirPath, pool);
  try {
    fs.unlinkSync(poolPath);
  } catch (err) {
    const mapped = mapFsError(err, poolPath, "failed to delete pool");
    validateExpectError(step.expect, mapped, index, stepId);
    return;
  }
  validateExpectError(step.expect, null, index, stepId);
}

async function runSpawnPoke(repoRoot, workdirPath, step, index, stepId) {
  const pool = requirePool(step, index, stepId);
  const input = requireInput(step, index, stepId);
  const messages = input.messages;
  if (!Array.isArray(messages)) {
    throw stepError(index, stepId, "input.messages must be array");
  }

  const plasmiteBin = resolvePlasmiteBin(repoRoot);

  const tasks = messages.map((message) => {
    if (!message || message.data === undefined) {
      throw stepError(index, stepId, "message.data is required");
    }
    const payload = JSON.stringify(message.data);
    const descrips = message.descrips ?? [];
    if (!Array.isArray(descrips)) {
      throw stepError(index, stepId, "message.descrips must be array");
    }
    return new Promise((resolve, reject) => {
      const child = spawn(
        plasmiteBin,
        ["--dir", workdirPath, "poke", pool, payload, ...flattenDescrips(descrips)],
        { stdio: "inherit" }
      );
      child.on("error", reject);
      child.on("exit", (code) => {
        if (code === 0) {
          resolve();
          return;
        }
        reject(new Error(`poke process failed: ${code}`));
      });
    });
  });

  try {
    await Promise.all(tasks);
  } catch (err) {
    throw stepError(index, stepId, err.message ?? String(err));
  }
}

function runCorruptPoolHeader(workdirPath, step, index, stepId) {
  const pool = requirePool(step, index, stepId);
  const pathToPool = resolvePoolPath(workdirPath, pool);
  fs.writeFileSync(pathToPool, "NOPE");
}

function runChmodPath(step, index, stepId) {
  if (process.platform === "win32") {
    throw stepError(index, stepId, "chmod_path is not supported on this platform");
  }
  const input = requireInput(step, index, stepId);
  const pathValue = input.path;
  const modeValue = input.mode;
  if (!pathValue) {
    throw stepError(index, stepId, "missing input.path");
  }
  if (!modeValue) {
    throw stepError(index, stepId, "missing input.mode");
  }
  fs.chmodSync(pathValue, parseInt(modeValue, 8));
}

function listPoolNames(workdirPath) {
  try {
    const entries = fs.readdirSync(workdirPath);
    return entries
      .filter((entry) => entry.endsWith(".plasmite"))
      .map((entry) => entry.replace(/\.plasmite$/, ""));
  } catch (err) {
    throw mapFsError(err, workdirPath, "failed to read pool directory");
  }
}

function mapFsError(err, targetPath, message) {
  const code = err?.code;
  let kind = "Io";
  if (code === "ENOENT") {
    kind = "NotFound";
  } else if (code === "EACCES" || code === "EPERM") {
    kind = "Permission";
  }
  return makePlasmiteError({ kind, message, path: targetPath });
}

function parseErrorJSON(output) {
  if (!output) {
    return new Error("plasmite error: kind=Internal; message=error");
  }
  let payload = {};
  try {
    payload = JSON.parse(output);
  } catch (err) {
    return new Error(`plasmite error: kind=Internal; message=${String(err)}`);
  }
  const errObj = payload.error ?? {};
  return makePlasmiteError({
    kind: errObj.kind ?? "Internal",
    message: errObj.message ?? "error",
    path: errObj.path,
    seq: errObj.seq,
    offset: errObj.offset,
  });
}

function makePlasmiteError({ kind, message, path, seq, offset }) {
  const details = [`kind=${kind}`, `message=${message}`];
  if (path) details.push(`path=${path}`);
  if (seq !== undefined && seq !== null) details.push(`seq=${seq}`);
  if (offset !== undefined && offset !== null) details.push(`offset=${offset}`);
  return new Error(`plasmite error: ${details.join("; ")}`);
}

function expectBounds(expected, actual, index, stepId) {
  const actualOldest = actual?.oldest ?? null;
  const actualNewest = actual?.newest ?? null;
  if ("oldest" in expected && expected.oldest !== actualOldest) {
    throw stepError(index, stepId, "bounds.oldest mismatch");
  }
  if ("newest" in expected && expected.newest !== actualNewest) {
    throw stepError(index, stepId, "bounds.newest mismatch");
  }
}

function expectedMessages(expect, index, stepId) {
  if (!expect) {
    throw stepError(index, stepId, "missing expect");
  }
  if (expect.messages && expect.messages_unordered) {
    throw stepError(index, stepId, "expect.messages and expect.messages_unordered are mutually exclusive");
  }
  if (Array.isArray(expect.messages)) {
    return { ordered: true, messages: expect.messages };
  }
  if (Array.isArray(expect.messages_unordered)) {
    return { ordered: false, messages: expect.messages_unordered };
  }
  throw stepError(index, stepId, "expect.messages or expect.messages_unordered is required");
}

function validateExpectError(expect, err, index, stepId) {
  if (!expect || !expect.error) {
    if (!err) return;
    throw stepError(index, stepId, `unexpected error: ${err.message ?? err}`);
  }
  if (!err) {
    throw stepError(index, stepId, "expected error but operation succeeded");
  }
  const parsed = parseError(err);
  if (!parsed.kind) {
    throw stepError(index, stepId, "unexpected error type");
  }
  if (parsed.kind !== expect.error.kind) {
    throw stepError(index, stepId, `expected error kind ${expect.error.kind}, got ${parsed.kind}`);
  }
  if (expect.error.message_contains && !parsed.message.includes(expect.error.message_contains)) {
    throw stepError(index, stepId, `expected message to contain '${expect.error.message_contains}', got '${parsed.message}'`);
  }
  if (typeof expect.error.has_path === "boolean" && expect.error.has_path !== parsed.hasPath) {
    throw stepError(index, stepId, "path presence mismatch");
  }
  if (typeof expect.error.has_seq === "boolean" && expect.error.has_seq !== parsed.hasSeq) {
    throw stepError(index, stepId, "seq presence mismatch");
  }
  if (typeof expect.error.has_offset === "boolean" && expect.error.has_offset !== parsed.hasOffset) {
    throw stepError(index, stepId, "offset presence mismatch");
  }
}

function parseError(err) {
  const message = err?.message ?? String(err);
  const kindMatch = message.match(/kind=([^;]+)/);
  const msgMatch = message.match(/message=([^;]+)/);
  const pathMatch = message.match(/path=([^;]+)/);
  const seqMatch = message.match(/seq=([^;]+)/);
  const offsetMatch = message.match(/offset=([^;]+)/);
  return {
    kind: kindMatch?.[1],
    message: msgMatch?.[1] ?? message,
    hasPath: Boolean(pathMatch),
    hasSeq: Boolean(seqMatch),
    hasOffset: Boolean(offsetMatch),
  };
}

function requirePool(step, index, stepId) {
  if (!step.pool) {
    throw stepError(index, stepId, "missing pool");
  }
  return step.pool;
}

function requireInput(step, index, stepId) {
  if (!step.input) {
    throw stepError(index, stepId, "missing input");
  }
  return step.input;
}

function resolvePoolPath(workdirPath, pool) {
  if (pool.includes("/")) {
    return pool;
  }
  if (pool.endsWith(".plasmite")) {
    return path.join(workdirPath, pool);
  }
  return path.join(workdirPath, `${pool}.plasmite`);
}

function resolvePlasmiteBin(repoRoot) {
  if (process.env.PLASMITE_BIN) {
    return process.env.PLASMITE_BIN;
  }
  const candidate = path.join(repoRoot, "target", "debug", "plasmite");
  if (fs.existsSync(candidate)) {
    return candidate;
  }
  throw new Error("plasmite binary not found; set PLASMITE_BIN or build target/debug/plasmite");
}

function flattenDescrips(descrips) {
  const out = [];
  descrips.forEach((value) => {
    out.push("--descrip", value);
  });
  return out;
}

function deepEqual(a, b) {
  return JSON.stringify(normalize(a)) === JSON.stringify(normalize(b));
}

function normalize(value) {
  if (Array.isArray(value)) {
    return value.map((entry) => normalize(entry));
  }
  if (value && typeof value === "object") {
    const keys = Object.keys(value).sort();
    const out = {};
    keys.forEach((key) => {
      out[key] = normalize(value[key]);
    });
    return out;
  }
  return value;
}

function stepError(index, stepId, message) {
  let out = `step ${index}`;
  if (stepId) {
    out += ` (${stepId})`;
  }
  return new Error(`${out}: ${message}`);
}

function tryCall(fn) {
  try {
    return { value: fn(), error: null };
  } catch (err) {
    return { value: null, error: err };
  }
}

main().catch((err) => {
  console.error(err.message ?? err);
  process.exit(1);
});
