/*
Purpose: Exercise Node binding behaviors beyond conformance manifests.
Key Exports: None (node:test suite).
Role: Validate tail timeout, large payloads, and lifecycle errors.
Invariants: Requires libplasmite + index.node to be built.
Notes: Uses temporary directories for isolated pools.
*/

const test = require("node:test");
const assert = require("node:assert/strict");
const childProcess = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");

const {
  Client,
  DEFAULT_POOL_SIZE_BYTES,
  Durability,
  ErrorKind,
  Pool,
  parseMessage,
  PlasmiteNativeError,
  RemoteClient,
  RemotePool,
} = require("../index.js");

function makeTempDir() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-node-"));
}

const TEST_POOL_SIZE_BYTES = 1024 * 1024;

function withClient(run) {
  const temp = makeTempDir();
  const poolDir = path.join(temp, "pools");
  fs.mkdirSync(poolDir, { recursive: true });
  const client = new Client(poolDir);
  try {
    return run({ client, poolDir, temp });
  } finally {
    client.close();
  }
}

function withPool(poolName, run, sizeBytes = TEST_POOL_SIZE_BYTES) {
  return withClient(({ client, poolDir, temp }) => {
    const pool = client.createPool(poolName, sizeBytes);
    try {
      return run({ client, pool, poolDir, temp });
    } finally {
      pool.close();
    }
  });
}

async function withPoolAsync(poolName, run, sizeBytes = TEST_POOL_SIZE_BYTES) {
  const temp = makeTempDir();
  const poolDir = path.join(temp, "pools");
  fs.mkdirSync(poolDir, { recursive: true });
  const client = new Client(poolDir);
  const pool = client.createPool(poolName, sizeBytes);
  try {
    return await run({ client, pool, poolDir, temp });
  } finally {
    pool.close();
    client.close();
  }
}

test("append/get supports large payload and tags", () => {
  withPool("big", ({ pool }) => {
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
  });
});

test("default pool size constant remains 1 MiB", () => {
  assert.equal(DEFAULT_POOL_SIZE_BYTES, 1024 * 1024);
});

test("parseMessage accepts plain envelope objects", () => {
  const message = parseMessage({
    seq: "7",
    time: "2026-02-18T00:00:00Z",
    data: { ok: true },
    meta: { tags: ["alpha"] },
  });
  assert.equal(message.seq, 7n);
  assert.equal(message.timeRfc3339, "2026-02-18T00:00:00Z");
  assert.deepEqual(message.tags, ["alpha"]);
});

test("append/get aliases and parseMessage helper round-trip JSON", () => {
  withPool("aliases", ({ pool }) => {
    const parsed = pool.append({ kind: "alias", ok: true }, ["alpha"]);
    assert.equal(parsed.data.kind, "alias");
    assert.equal(parsed.data.ok, true);
    assert.deepEqual(parsed.meta.tags, ["alpha"]);
    assert.ok(Buffer.isBuffer(parsed.raw));
    assert.ok(parsed.time instanceof Date);
    assert.equal(typeof parsed.timeRfc3339, "string");

    const fetched = pool.get(parsed.seq);
    assert.equal(fetched.data.kind, "alias");
    assert.equal(fetched.data.ok, true);
  });
});

test("client.pool creates missing pool and reopens existing pool", () => {
  withClient(({ client }) => {
    const first = client.pool("work", TEST_POOL_SIZE_BYTES);
    const second = client.pool("work", 2 * TEST_POOL_SIZE_BYTES);
    const appended = first.append({ kind: "created" }, ["alpha"]);
    const fetched = second.get(appended.seq);
    assert.equal(fetched.data.kind, "created");
    first.close();
    second.close();
  });
});

test("default client creates pool dir in fresh HOME", () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-node-home-"));
  const expectedPoolDir = path.join(home, ".plasmite", "pools");
  const expectedPoolPath = path.join(expectedPoolDir, "work.plasmite");
  assert.equal(fs.existsSync(expectedPoolDir), false);

  const script = `
    const { Client } = require("./index.js");
    const client = new Client();
    const pool = client.pool("work", ${TEST_POOL_SIZE_BYTES});
    pool.append({ kind: "one" }, ["alpha"]);
    pool.close();
    client.close();
  `;
  const output = childProcess.spawnSync(process.execPath, ["-e", script], {
    cwd: path.join(__dirname, ".."),
    env: { ...process.env, HOME: home },
    encoding: "utf8",
  });
  assert.equal(output.status, 0, output.stderr || output.stdout);
  assert.equal(fs.existsSync(expectedPoolPath), true);
});

test("tail timeout returns no message and close is safe", () => {
  withPool("tail", ({ pool }) => {
    const stream = pool.openStream(BigInt(9999), BigInt(1), BigInt(10));
    const next = stream.nextJson();
    assert.equal(next, null);
    stream.close();
    stream.close();
  });
});

test("stream and lite3 stream support for-of iteration", () => {
  withPool("iter", ({ pool }) => {
    const first = pool.append({ kind: "one" }, ["iter"]);
    pool.append({ kind: "two" }, ["iter"]);
    pool.append({ kind: "three" }, ["iter"]);

    const stream = pool.openStream(first.seq, BigInt(3), BigInt(50));
    const seenKinds = [];
    for (const message of stream) {
      seenKinds.push(parseMessage(message).data.kind);
    }
    assert.deepEqual(seenKinds, ["one", "two", "three"]);
    assert.equal(stream.nextJson(), null);
    stream.close();

    const frameSeed = pool.getLite3(first.seq);
    assert.ok(frameSeed.time instanceof Date);
    const liteSeq = pool.appendLite3(frameSeed.payload);
    pool.appendLite3(frameSeed.payload);
    const lite3Stream = pool.openLite3Stream(liteSeq, BigInt(2), BigInt(50));
    const frameSeqs = [];
    for (const frame of lite3Stream) {
      frameSeqs.push(frame.seq);
    }
    assert.deepEqual(frameSeqs, [liteSeq, liteSeq + 1n]);
    assert.equal(lite3Stream.next(), null);
    lite3Stream.close();
  });
});

test("tail filters by tags and replay works as a pool method", async () => {
  await withPoolAsync("tail-replay", async ({ pool }) => {
    pool.append({ kind: "drop", i: 1 }, ["drop"]);
    const keep1 = pool.append({ kind: "keep", i: 2 }, ["keep"]);
    pool.append({ kind: "drop", i: 3 }, ["drop"]);
    const keep2 = pool.append({ kind: "keep", i: 4 }, ["keep"]);

    const tailed = [];
    for await (const message of pool.tail({ tags: ["keep"], maxMessages: 2, timeoutMs: 10 })) {
      tailed.push(message.data.i);
    }
    assert.deepEqual(tailed, [2, 4]);

    const replayed = [];
    for await (const message of pool.replay({
      sinceSeq: keep1.seq,
      maxMessages: 2,
      speed: 2.0,
      tags: ["keep"],
    })) {
      replayed.push(message.seq);
    }
    assert.deepEqual(replayed, [keep1.seq, keep2.seq]);
  });
});

test("tail timeout with no messages returns done", async () => {
  await withPoolAsync("tail-empty", async ({ pool }) => {
    const iterator = pool
      .tail({ sinceSeq: 9999, maxMessages: 1, timeoutMs: 10, tags: ["nope"] })
      [Symbol.asyncIterator]();
    const next = await iterator.next();
    assert.equal(next.done, true);
    assert.equal(next.value, undefined);
  });
});

test("closed pool/client reject operations", () => {
  withClient(({ client }) => {
    const pool = client.createPool("closed", TEST_POOL_SIZE_BYTES);
    pool.close();
    assert.throws(() => pool.appendJson(Buffer.from("{}"), [], Durability.Fast));
    client.close();
    assert.throws(() => client.createPool("other", TEST_POOL_SIZE_BYTES));
  });
});

test("native errors expose structured metadata", () => {
  withClient(({ client }) => {
    client.close();

    let captured;
    try {
      client.createPool("other", TEST_POOL_SIZE_BYTES);
    } catch (err) {
      captured = err;
    }

    assert.ok(captured instanceof PlasmiteNativeError);
    assert.equal(captured.kind, ErrorKind.Usage);
    assert.match(captured.message, /kind=Usage/);
  });
});

test("native errors with unknown kind default to Io", () => {
  const pool = new Pool({
    appendJson() {
      throw new Error("plasmite error: kind=NoSuchKind; message=bad");
    },
  });

  assert.throws(
    () => pool.appendJson(Buffer.from("{}"), [], Durability.Fast),
    (err) => {
      assert.ok(err instanceof PlasmiteNativeError);
      assert.equal(err.kind, ErrorKind.Io);
      return true;
    }
  );
});

test("lite3 append/get/tail round-trips bytes", () => {
  withPool("lite3", ({ pool }) => {
    const payload = { x: 1 };
    const messageBuf = pool.appendJson(
      Buffer.from(JSON.stringify(payload)),
      ["alpha"],
      Durability.Fast
    );
    const message = JSON.parse(messageBuf.toString("utf8"));
    const lite3Frame = pool.getLite3(BigInt(message.seq));
    assert.ok(lite3Frame.time instanceof Date);
    assert.ok(lite3Frame.payload.length > 0);

    const seq2 = pool.appendLite3(lite3Frame.payload);
    const lite3Frame2 = pool.getLite3(seq2);
    assert.deepEqual(lite3Frame2.payload, lite3Frame.payload);

    const stream = pool.openLite3Stream(seq2, BigInt(1), BigInt(50));
    const next = stream.next();
    assert.ok(next);
    assert.equal(next.seq, seq2);
    assert.deepEqual(next.payload, lite3Frame.payload);
    assert.equal(stream.next(), null);
    stream.close();
  });
});

test("lite3 append rejects invalid payloads", () => {
  withPool("lite3-bad", ({ pool }) => {
    assert.throws(() => pool.appendLite3(Buffer.from([0x01]), Durability.Fast));
  });
});

test("remote tail encodes tags as repeated tag query params", async () => {
  const originalFetch = global.fetch;
  let capturedUrl = null;

  global.fetch = async (input) => {
    capturedUrl = new URL(String(input));
    return {
      ok: true,
      status: 200,
      body: new ReadableStream({
        start(controller) {
          controller.enqueue(
            new TextEncoder().encode(
              '{"seq":1,"time":"2026-01-01T00:00:00Z","meta":{"tags":["keep","prod"]},"data":{}}\n'
            )
          );
          controller.close();
        },
      }),
    };
  };

  try {
    const client = new RemoteClient("http://127.0.0.1:9700");
    const pool = new RemotePool(client, "demo");
    for await (const message of pool.tail({
      tags: ["keep", "prod"],
      maxMessages: 1,
      timeoutMs: 10,
    })) {
      assert.equal(message.seq, 1n);
      assert.ok(message.time instanceof Date);
      assert.deepEqual(message.tags, ["keep", "prod"]);
      break;
    }

    assert.ok(capturedUrl, "expected fetch URL to be captured");
    assert.equal(capturedUrl.pathname, "/v0/pools/demo/tail");
    assert.deepEqual(capturedUrl.searchParams.getAll("tag"), ["keep", "prod"]);
    assert.equal(capturedUrl.searchParams.get("max"), "1");
    assert.equal(capturedUrl.searchParams.get("timeout_ms"), "10");
  } finally {
    global.fetch = originalFetch;
  }
});

test("remote errors with unknown kind map to Io", async () => {
  const originalFetch = global.fetch;
  global.fetch = async () => ({
    ok: false,
    status: 400,
    async json() {
      return {
        error: {
          kind: "NoSuchKind",
          message: "bad request",
        },
      };
    },
  });

  try {
    const client = new RemoteClient("http://127.0.0.1:9700");
    const pool = new RemotePool(client, "demo");
    await assert.rejects(
      pool.append({ value: 1 }),
      (err) => {
        assert.equal(err.kind, ErrorKind.Io);
        return true;
      }
    );
  } finally {
    global.fetch = originalFetch;
  }
});

test("remote append rejects invalid durability before fetch", async () => {
  const originalFetch = global.fetch;
  let called = false;
  global.fetch = async () => {
    called = true;
    throw new Error("unexpected fetch");
  };

  try {
    const client = new RemoteClient("http://127.0.0.1:9700");
    const pool = new RemotePool(client, "demo");
    await assert.rejects(
      pool.append({ value: 1 }, [], "bogus"),
      /durability must be Durability\.Fast or Durability\.Flush/
    );
    assert.equal(called, false);
  } finally {
    global.fetch = originalFetch;
  }
});
