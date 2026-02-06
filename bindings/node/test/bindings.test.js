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

test("append/get supports large payload and tags", () => {
  const temp = makeTempDir();
  const poolDir = path.join(temp, "pools");
  fs.mkdirSync(poolDir, { recursive: true });
  const client = new Client(poolDir);
  const pool = client.createPool("big", 1024 * 1024);

  const payload = { blob: "x".repeat(64 * 1024) };
  const tags = ["alpha", "beta", "gamma"];
  const messageBuf = pool.appendJson(
    Buffer.from(JSON.stringify(payload)),
    tags,
    Durability.Fast
  );
  const message = JSON.parse(messageBuf.toString("utf8"));
  assert.equal(message.data.blob.length, payload.blob.length);
  assert.deepEqual(message.meta.tags, tags);

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

test("lite3 append/get/tail round-trips bytes", () => {
  const temp = makeTempDir();
  const poolDir = path.join(temp, "pools");
  fs.mkdirSync(poolDir, { recursive: true });
  const client = new Client(poolDir);
  const pool = client.createPool("lite3", 1024 * 1024);

  const payload = { x: 1 };
  const messageBuf = pool.appendJson(
    Buffer.from(JSON.stringify(payload)),
    ["alpha"],
    Durability.Fast
  );
  const message = JSON.parse(messageBuf.toString("utf8"));
  const lite3Frame = pool.getLite3(BigInt(message.seq));
  assert.ok(lite3Frame.payload.length > 0);

  const seq2 = pool.appendLite3(lite3Frame.payload, Durability.Fast);
  const lite3Frame2 = pool.getLite3(seq2);
  assert.deepEqual(lite3Frame2.payload, lite3Frame.payload);

  const stream = pool.openLite3Stream(seq2, BigInt(1), BigInt(50));
  const next = stream.next();
  assert.ok(next);
  assert.equal(next.seq, seq2);
  assert.deepEqual(next.payload, lite3Frame.payload);
  assert.equal(stream.next(), null);
  stream.close();

  pool.close();
  client.close();
});

test("lite3 append rejects invalid payloads", () => {
  const temp = makeTempDir();
  const poolDir = path.join(temp, "pools");
  fs.mkdirSync(poolDir, { recursive: true });
  const client = new Client(poolDir);
  const pool = client.createPool("lite3-bad", 1024 * 1024);

  assert.throws(() => pool.appendLite3(Buffer.from([0x01]), Durability.Fast));

  pool.close();
  client.close();
});
