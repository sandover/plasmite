/*
Purpose: Exercise Node binding behaviors beyond conformance manifests.
Key Exports: None (node:test suite).
Role: Validate tail timeout, large payloads, and lifecycle errors.
Invariants: Requires libplasmite + index.node to be built.
Notes: Uses temporary directories for isolated pools.
*/

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");

const { Client, Durability } = require("../index.js");

function makeTempDir() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-node-"));
}

test("append/get supports large payload and descrips", () => {
  const temp = makeTempDir();
  const poolDir = path.join(temp, "pools");
  fs.mkdirSync(poolDir, { recursive: true });
  const client = new Client(poolDir);
  const pool = client.createPool("big", 1024 * 1024);

  const payload = { blob: "x".repeat(64 * 1024) };
  const descrips = ["alpha", "beta", "gamma"];
  const messageBuf = pool.appendJson(
    Buffer.from(JSON.stringify(payload)),
    descrips,
    Durability.Fast
  );
  const message = JSON.parse(messageBuf.toString("utf8"));
  assert.equal(message.data.blob.length, payload.blob.length);
  assert.deepEqual(message.meta.descrips, descrips);

  const fetchedBuf = pool.getJson(BigInt(message.seq));
  const fetched = JSON.parse(fetchedBuf.toString("utf8"));
  assert.equal(fetched.data.blob.length, payload.blob.length);

  pool.close();
  client.close();
});

test("tail timeout returns no message and close is safe", () => {
  const temp = makeTempDir();
  const poolDir = path.join(temp, "pools");
  fs.mkdirSync(poolDir, { recursive: true });
  const client = new Client(poolDir);
  const pool = client.createPool("tail", 1024 * 1024);

  const stream = pool.openStream(BigInt(9999), BigInt(1), BigInt(10));
  const next = stream.nextJson();
  assert.equal(next, null);
  stream.close();
  stream.close();

  pool.close();
  client.close();
});

test("closed pool/client reject operations", () => {
  const temp = makeTempDir();
  const poolDir = path.join(temp, "pools");
  fs.mkdirSync(poolDir, { recursive: true });
  const client = new Client(poolDir);
  const pool = client.createPool("closed", 1024 * 1024);

  pool.close();
  assert.throws(() => pool.appendJson(Buffer.from("{}"), [], Durability.Fast));

  client.close();
  assert.throws(() => client.createPool("other", 1024 * 1024));
});
