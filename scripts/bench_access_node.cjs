#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const readline = require("node:readline");
const { isDeepStrictEqual } = require("node:util");

function usage() {
  throw new Error(
    "usage: bench_access_node.cjs local|http|https ROOT SERVER_OR_DIR POOL [KEY_FILE]",
  );
}

function readSecret(file) {
  const contents = fs.readFileSync(file, "utf8").trim();
  let key = contents;
  try {
    const parsed = JSON.parse(contents);
    if (parsed && typeof parsed.access_key === "string") key = parsed.access_key;
  } catch {}
  const parts = key.split(".");
  if (parts.length !== 3 || parts[0] !== "pk1" || !parts[2]) {
    throw new Error("key file must contain a pk1 access key");
  }
  return parts[2];
}

function localPoolFile(directory, pool) {
  const file = path.isAbsolute(pool)
    ? pool
    : path.join(directory, pool.endsWith(".plasmite") ? pool : `${pool}.plasmite`);
  return file;
}

function number(value, label) {
  const result = typeof value === "bigint" ? Number(value) : value;
  if (!Number.isSafeInteger(result)) throw new Error(`${label} exceeds JavaScript's safe integer range`);
  return result;
}

function now() {
  return process.hrtime.bigint();
}

function elapsed(start) {
  return Number(now() - start);
}

async function main() {
  const [mode, root, serverOrDir, poolName, keyFile, ...extra] = process.argv.slice(2);
  if (!mode || !root || !serverOrDir || !poolName || extra.length || !["local", "http", "https"].includes(mode)) usage();
  if ((mode === "https") !== Boolean(keyFile)) usage();

  const { Client, Durability, RemoteClient } = require(path.join(path.resolve(root), "bindings/node"));
  let client;
  let pool;
  if (mode === "local") {
    client = new Client(serverOrDir);
    pool = client.openPool(poolName);
  } else {
    const options = mode === "https" ? { token: readSecret(keyFile) } : {};
    client = new RemoteClient(serverOrDir, options);
    pool = await client.openPool(poolName);
    await client.poolInfo(poolName); // Warm the HTTP connection before timed calls.
  }

  async function handle(request) {
    const started = now();
    if (request.op === "info") {
      const fileSize = mode === "local"
        ? fs.statSync(localPoolFile(serverOrDir, poolName)).size
        : (await client.poolInfo(poolName)).file_size;
      return { elapsed_ns: elapsed(started), latencies_ns: [], result: { file_size: fileSize } };
    }

    if (request.op === "append") {
      if (!Array.isArray(request.items)) throw new Error("append needs items");
      const latencies = [];
      const sequences = [];
      for (const item of request.items) {
        if (!item || typeof item !== "object" || Array.isArray(item)) {
          throw new Error("append items must be JSON objects");
        }
        const callStarted = now();
        const message = mode === "local"
          ? pool.append(item, [], Durability.Fast)
          : await pool.append(item, [], "fast");
        latencies.push(elapsed(callStarted));
        sequences.push(number(message.seq, "sequence"));
      }
      return { elapsed_ns: elapsed(started), latencies_ns: latencies, result: { sequences } };
    }

    if (request.op === "read") {
      const { sequences, expected } = request;
      if (!Array.isArray(sequences) || !Array.isArray(expected) || sequences.length !== expected.length) {
        throw new Error("read needs matching sequences and expected arrays");
      }
      const latencies = [];
      for (let index = 0; index < sequences.length; index += 1) {
        const seq = sequences[index];
        if (!Number.isSafeInteger(seq) || seq < 0) throw new Error("sequences must be safe nonnegative integers");
        const callStarted = now();
        const message = mode === "local" ? pool.get(seq) : await pool.get(seq);
        latencies.push(elapsed(callStarted));
        if (number(message.seq, "sequence") !== seq || !isDeepStrictEqual(message.data, expected[index])) {
          throw new Error(`readback mismatch for sequence ${seq}`);
        }
      }
      return {
        elapsed_ns: elapsed(started),
        latencies_ns: latencies,
        result: { verified_count: sequences.length },
      };
    }

    throw new Error("op must be info, append, or read");
  }

  const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
  try {
    for await (const line of input) {
      try {
        const response = await handle(JSON.parse(line));
        process.stdout.write(`${JSON.stringify(response)}\n`);
      } catch (error) {
        process.stderr.write(`${error.message || error}\n`);
        process.exitCode = 1;
        break;
      }
    }
  } finally {
    input.close();
    if (mode === "local") {
      pool.close();
      client.close();
    }
  }
}

main().catch((error) => {
  process.stderr.write(`${error.message || error}\n`);
  process.exitCode = 1;
});
