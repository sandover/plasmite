/*
Purpose: Exercise the Node binding against native pools and remote servers.
Key Exports: None (node:test suite).
Role: Validate tail timeout, large payloads, and lifecycle errors.
Invariants: Requires libplasmite + index.node to be built.
Notes: Uses temporary directories for isolated pools.
*/

const test = require("node:test");
const assert = require("node:assert/strict");
const childProcess = require("node:child_process");
const dgram = require("node:dgram");
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
  UdpClient,
  UdpFollow,
  UdpPool,
  RemoteClient,
  RemoteError,
  RemotePool,
} = require("../index.js");

async function feedForTest(pool, value, tags) {
  return parseMessage(await pool.feedJson(Buffer.from(JSON.stringify(value)), { tags }));
}

function makeTempDir() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-node-"));
}

const TEST_POOL_SIZE_BYTES = 1024 * 1024;

async function withClient(run) {
  const temp = makeTempDir();
  const poolDir = path.join(temp, "pools");
  fs.mkdirSync(poolDir, { recursive: true });
  const client = new Client(poolDir);
  try {
    return await run({ client, poolDir, temp });
  } finally {
    client.close();
    fs.rmSync(temp, { recursive: true, force: true });
  }
}

async function withPool(poolName, run, sizeBytes = TEST_POOL_SIZE_BYTES) {
  return withClient(async ({ client, poolDir, temp }) => {
    const pool = client.createPool(poolName, sizeBytes);
    try {
      return await run({ client, pool, poolDir, temp });
    } finally {
      pool.close();
    }
  });
}

async function fillPast(pool, seq) {
  for (let i = 0; i < 10000; i += 1) {
    await feedForTest(pool, { padding: "x".repeat(4096), i });
    try {
      parseMessage(pool._getJson(seq));
    } catch (err) {
      if (err instanceof PlasmiteNativeError && err.kind === ErrorKind.RetentionGap) {
        return;
      }
      throw err;
    }
  }
  assert.fail(`sequence ${seq} remained retained after filling the pool`);
}

test("native output buffers and slices survive handle close and garbage collection", async () => {
  const script = `
(async () => {
    const assert = require('node:assert/strict');
    const fs = require('node:fs'), os = require('node:os'), path = require('node:path');
    const { Client, parseMessage } = require(${JSON.stringify(path.resolve(__dirname, "../index.js"))});
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-owned-'));
    try {
      const client = new Client(dir), pool = client.createPool('owned', 1048576n);
      const message = parseMessage(await pool.feedJson(Buffer.from(JSON.stringify({text:'x'.repeat(4096)}))));
      let frame = pool._getLite3(message.seq);
      const expected = Buffer.from(frame.payload);
      const slice = frame.payload.subarray(1);
      const json = pool._getJson(message.seq);
      const other = pool._getLite3(message.seq).payload;
      other.fill(0);
      assert.deepEqual(frame.payload, expected);
      frame = null;
      pool.close(); client.close();
      for (let i=0;i<8;i++) global.gc();
      assert.deepEqual(slice, expected.subarray(1));
      assert.equal(JSON.parse(json).data.text.length, 4096);
    } finally { fs.rmSync(dir,{recursive:true,force:true}); }

})().catch(error => { console.error(error); process.exitCode = 1; });
`;
  const result = childProcess.spawnSync(process.execPath, ["--expose-gc", "-e", script], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
});

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
    fs.rmSync(temp, { recursive: true, force: true });
  }
}

test("append/get supports large payload and tags", async () => {
  await withPool("big", async ({ pool }) => {
    const payload = { blob: "0123456789abcdef".repeat(4096) };
    const tags = ["alpha", "beta", "gamma"];
    const messageBuf = await pool.feedJson(
      Buffer.from(JSON.stringify(payload)),
      { tags, durability: Durability.Fast }
    );
    const message = JSON.parse(messageBuf.toString("utf8"));
    assert.deepEqual(message.data, payload);
    assert.deepEqual(message.meta.tags, tags);

    const fetchedBuf = pool._getJson(BigInt(message.seq));
    const fetched = JSON.parse(fetchedBuf.toString("utf8"));
    assert.deepEqual(fetched.data, payload);
  });
});

test("default pool size constant remains 1 MiB", async () => {
  assert.equal(DEFAULT_POOL_SIZE_BYTES, 1024 * 1024);
});

test("parseMessage accepts plain envelope objects", async () => {
  const message = parseMessage({
    seq: "7",
    time: "2026-02-18T00:00:00Z",
    data: { ok: true },
    meta: { tags: ["alpha"] },
  });
  assert.equal(message.seq, 7n);
  assert.equal(message.time, BigInt(Date.parse("2026-02-18T00:00:00Z")) * 1_000_000n);
  assert.equal(JSON.parse(message.raw).time, "2026-02-18T00:00:00.000000000Z");
  assert.deepEqual(message.tags, ["alpha"]);
});

test("encoded feed and parseMessage round-trip JSON", async () => {
  await withPool("aliases", async ({ pool }) => {
    const parsed = await feedForTest(pool, { kind: "alias", ok: true }, ["alpha"]);
    assert.equal(parsed.data.kind, "alias");
    assert.equal(parsed.data.ok, true);
    assert.deepEqual(parsed.meta.tags, ["alpha"]);
    assert.ok(Buffer.isBuffer(parsed.raw));
    assert.equal(typeof parsed.time, "bigint");

    const fetched = parseMessage(pool._getJson(parsed.seq));
    assert.equal(fetched.data.kind, "alias");
    assert.equal(fetched.data.ok, true);
  });
});

test("openPool with create preserves existing pools", async () => {
  await withClient(async ({ client }) => {
    const first = client.openPool("work", { create: true });
    const second = client.openPool("work", { create: true });
    const appended = await feedForTest(first, { kind: "created" }, ["alpha"]);
    const fetched = parseMessage(second._getJson(appended.seq));
    assert.equal(fetched.data.kind, "created");
    first.close();
    second.close();
  });
});

test("default client creates pool dir in fresh HOME", async () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-node-home-"));
  const expectedPoolDir = path.join(home, ".plasmite", "pools");
  const expectedPoolPath = path.join(expectedPoolDir, "work.plasmite");
  assert.equal(fs.existsSync(expectedPoolDir), false);

  const script = `
(async () => {
    const { Client, parseMessage } = require("./index.js");
    const client = new Client();
    const pool = client.openPool("work", { create: true });
    await pool.feedJson(Buffer.from(JSON.stringify({ kind: "one" })), { tags: ["alpha"] });
    pool.close();
    client.close();

})().catch(error => { console.error(error); process.exitCode = 1; });
`;
  const output = childProcess.spawnSync(process.execPath, ["-e", script], {
    cwd: path.join(__dirname, ".."),
    env: { ...process.env, HOME: home, ...(process.platform === "win32" ? { USERPROFILE: home } : {}) },
    encoding: "utf8",
  });
  assert.equal(output.status, 0, output.stderr || output.stdout);
  assert.equal(fs.existsSync(expectedPoolPath), true);
  fs.rmSync(home, { recursive: true, force: true });
});

test("follow per-call wait expires and close is safe", async () => {
  await withPoolAsync("live", async ({ pool }) => {
    const reader = await pool.follow();
    assert.deepEqual(await reader.next(0.01), { done: false, value: undefined });
    await reader.return();
    await reader.return();
  });
});

test("text and Lite3 fetch readers share finite iteration", async () => {
  await withPoolAsync("iter", async ({ pool }) => {
    const first = await pool.feed({ kind: "one" }, { tags: ["iter"] });
    await pool.feed({ kind: "two" }, { tags: ["iter"] });
    await pool.feed({ kind: "three" }, { tags: ["iter"] });

    const seenKinds = [];
    for await (const message of await pool.fetch({ since: first.seq })) seenKinds.push(message.data.kind);
    assert.deepEqual(seenKinds, ["one", "two", "three"]);

    const seed = await pool.fetchLite3({ since: first.seq });
    const { value: frameSeed } = await seed.next();
    await seed.return();
    const liteSeq = (await pool.feedLite3Receipt(frameSeed.payload)).seq;
    await pool.feedLite3Receipt(frameSeed.payload);
    const frameSeqs = [];
    for await (const frame of await pool.fetchLite3({ since: liteSeq })) frameSeqs.push(frame.seq);
    assert.deepEqual(frameSeqs, [liteSeq, liteSeq + 1n]);
  });
});

test("fetch filters retained messages by tags", async () => {
  await withPoolAsync("tail-matching", async ({ pool }) => {
    await feedForTest(pool, { kind: "drop", i: 1 }, ["drop"]);
    await feedForTest(pool, { kind: "keep", i: 2 }, ["keep"]);
    await feedForTest(pool, { kind: "drop", i: 3 }, ["drop"]);
    await feedForTest(pool, { kind: "keep", i: 4 }, ["keep"]);

    const tailed = [];
    for await (const message of await pool.fetch({ tags: ["keep"] })) {
      tailed.push(message.data.i);
    }
    assert.deepEqual(tailed, [2, 4]);


  });
});

test("follow timeout keeps the reader open", async () => {
  await withPoolAsync("tail-empty", async ({ pool }) => {
    const reader = await pool.follow({ tags: ["keep"] });
    try {
      assert.deepEqual(await reader.next(0.01), { done: false, value: undefined });
      const message = await feedForTest(pool, { later: true }, ["keep"]);
      assert.equal((await reader.next(0.1)).value.seq, message.seq);
    } finally { reader.close(); }
  });
});

test("a quiet native tail leaves timers free to append and abort", async () => {
  await withPoolAsync("responsive-tail", async ({ pool }) => {
    let timer;
    try {
      timer = setTimeout(() => feedForTest(pool, { from: "timer" }), 10);
      const reader = await pool.follow({ since: "oldest" });
      const result = await reader.next(1);
      reader.close();
      assert.equal(result.done, false);
      assert.equal(result.value.data.from, "timer");
    } finally {
      clearTimeout(timer);
    }
    const controller = new AbortController();
    const tail = await pool.follow({ since: 2n, signal: controller.signal });
    const next = tail.next();
    const abortTimer = setTimeout(() => controller.abort(), 10);
    let deadline;
    try {
      const result = await Promise.race([next, new Promise((_, reject) => {
        deadline = setTimeout(() => reject(new Error("abort did not end quiet read")), 1000);
      })]);
      assert.equal(result.done, true);
    } finally {
      controller.abort();
      clearTimeout(abortTimer);
      clearTimeout(deadline);
      await tail.return();
    }
  });
});

test("worker waits preserve position and close keeps an active handle alive", async () => {
  await withPoolAsync("worker-waits", async ({ pool }) => {
    const stream = (await pool.follow({ since: 1n }))._stream;
    try {
      assert.equal((await stream.nextJsonWait(0)).status, 0);
      const message = await feedForTest(pool, { value: 7 });
      const read = await stream.nextJsonWait(0);
      assert.equal(read.status, 1);
      assert.equal(parseMessage(read.message).seq, message.seq);
      assert.equal((await stream.nextJsonWait(0)).status, 0);
      const raw = (await pool.fetchLite3({ since: 1n }))._stream;
      try {
        const result = await raw.nextWait(0);
        assert.equal(result.status, 1);
        assert.equal(result.frame.seq, message.seq);
        assert.ok(typeof result.frame.time === "bigint");
        assert.equal((await raw.nextWait(0)).status, 2);
      } finally { raw.close(); }
    } finally { stream.close(); }
    for (const [stream, wait] of [
      [(await pool.follow({ since: 2n }))._stream, (s) => s.nextJsonWait(5000)],
      [(await pool.followLite3({ since: 2n }))._stream, (s) => s.nextWait(5000)],
    ]) {
      const pending = wait(stream);
      await assert.rejects(wait(stream), /active reader/);
      stream.close();
      assert.equal((await pending).status, 3);
      stream.close();
    }
  });
});

test("local tail finishes at u64 maximum without reopening from zero", async () => {
  await withPoolAsync("last-sequence", async ({ pool, poolDir }) => {
    await feedForTest(pool, { last: true });
    const file = fs.openSync(path.join(poolDir, "last-sequence.plasmite"), "r+");
    const header = Buffer.alloc(104);
    fs.readSync(file, header, 0, header.length, 0);
    const last = (1n << 64n) - 1n;
    const bytes = Buffer.alloc(8);
    bytes.writeBigUInt64LE(last);
    try {
      for (const offset of [88, 96, Number(header.readBigUInt64LE(40)) + 16]) {
        fs.writeSync(file, bytes, 0, 8, offset);
      }
    } finally { fs.closeSync(file); }
    const messages = [];
    for await (const message of await pool.follow({ since: last })) messages.push(message.seq);
    assert.deepEqual(messages, [last]);
  });
});

test("a retention gap gives the caller the first retained sequence", async () => {
  await withPoolAsync("tail-gap", async ({ pool }) => {
    const first = await feedForTest(pool, { kind: "first" });
    await fillPast(pool, first.seq);
    for (const open of [pool.follow, pool.fetchLite3]) {
      await assert.rejects(open.call(pool, { since: first.seq, tags: ["never"] }), (err) => {
        assert.ok(err instanceof PlasmiteNativeError);
        assert.equal(err.kind, ErrorKind.RetentionGap);
        assert.equal(err.seq, first.seq);
        assert.ok(err.nextSeq > first.seq);
        return true;
      });
    }
  }, 64 * 1024);
});

test("an open stream reports the first unread sequence after overwrite", async () => {
  await withPoolAsync("slow-reader", async ({ pool }) => {
    const seed = await feedForTest(pool, { kind: "seed" });
    const reader = await pool.follow({ since: seed.seq });
    try {
      assert.equal((await reader.next()).value.seq, seed.seq);
      const lost = await feedForTest(pool, { kind: "lost" });
      await fillPast(pool, lost.seq);
      await assert.rejects(reader.next(), { kind: ErrorKind.RetentionGap, seq: lost.seq });
      assert.equal((await reader.next()).done, true);
    } finally { reader.close(); }
  }, 64 * 1024);
});

test("closed pool/client reject operations", async () => {
  await withClient(async ({ client }) => {
    const pool = client.createPool("closed", TEST_POOL_SIZE_BYTES);
    pool.close();
    await assert.rejects(() => pool.feedJson(Buffer.from("{}"), { durability: Durability.Fast }));
    client.close();
    assert.throws(() => client.createPool("other", TEST_POOL_SIZE_BYTES));
  });
});

test("native errors expose structured metadata", async () => {
  await withClient(async ({ client }) => {
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

test("native error paths do not become error metadata", async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "plasmite-native-error-"));
  const poolDir = path.join(directory, "pools; seq=99; message=injected");
  const client = new Client(poolDir);
  try {
    assert.throws(() => client.openPool("missing"), (err) => {
      assert.ok(err instanceof PlasmiteNativeError);
      assert.equal(err.kind, ErrorKind.NotFound);
      assert.equal(err.path, path.join(poolDir, "missing.plasmite"));
      assert.equal(err.seq, undefined);
      return true;
    });
  } finally {
    client.close();
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

test("missing sequences and corrupt pool files carry different error details", async () => {
  await withPool("damaged", async ({ client, pool, poolDir }) => {
    assert.throws(() => parseMessage(pool._getJson(1n)), {
      kind: ErrorKind.NotFound,
      seq: 1n,
      path: undefined,
    });
    pool.close();
    const poolPath = path.join(poolDir, "damaged.plasmite");
    fs.truncateSync(poolPath, 4);
    assert.throws(() => client.openPool("damaged"), {
      kind: ErrorKind.Corrupt,
      path: poolPath,
      seq: undefined,
    });
  });
});

test("native corrupt-frame errors retain a zero bigint offset", async () => {
  await withPool("offset-error", async ({ pool, poolDir }) => {
    await feedForTest(pool, { value: 1 });
    const filename = path.join(poolDir, "offset-error.plasmite");
    const header = fs.readFileSync(filename);
    const ring = Number(header.readBigUInt64LE(40));
    const file = fs.openSync(filename, "r+");
    try { fs.writeSync(file, Buffer.from("X"), 0, 1, ring); }
    finally { fs.closeSync(file); }
    assert.throws(() => parseMessage(pool._getJson(1n)), (err) => {
      assert.ok(err instanceof PlasmiteNativeError);
      assert.equal(err.kind, ErrorKind.Corrupt);
      assert.equal(err.offset, 0n);
      return true;
    });
  });
});

test("native missing-message errors retain exact uint64 sequences", async () => {
  await withPool("exact-errors", async ({ pool }) => {
    for (const seq of [0n, 9007199254740992n, 9007199254740993n, (1n << 64n) - 1n]) {
      assert.throws(() => parseMessage(pool._getJson(seq)), (err) => {
        assert.ok(err instanceof PlasmiteNativeError);
        assert.equal(err.seq, seq);
        assert.equal(err.offset, undefined);
        return true;
      });
    }
  });
});

test("native errors with unknown kind default to Io", async () => {
  const pool = new Pool({
    feedJson() {
      throw Object.assign(new Error("plasmite error: kind=NoSuchKind; message=bad"), { kind: "NoSuchKind" });
    },
  });

  await assert.rejects(
    () => pool.feedJson(Buffer.from("{}"), { durability: Durability.Fast }),
    (err) => {
      assert.ok(err instanceof PlasmiteNativeError);
      assert.equal(err.kind, ErrorKind.Io);
      return true;
    }
  );
});

test("Lite3 feed and fetch round-trip bytes", async () => {
  await withPoolAsync("lite3", async ({ pool }) => {
    const payload = { x: 1 };
    const messageBuf = await pool.feedJson(
      Buffer.from(JSON.stringify(payload)),
      { tags: ["alpha"], durability: Durability.Fast }
    );
    const message = JSON.parse(messageBuf.toString("utf8"));
    const lite3Frame = pool._getLite3(BigInt(message.seq));
    assert.equal(typeof lite3Frame.time, "bigint");
    assert.ok(lite3Frame.payload.length > 0);

    const seq2 = (await pool.feedLite3Receipt(lite3Frame.payload)).seq;
    const lite3Frame2 = pool._getLite3(seq2);
    assert.deepEqual(lite3Frame2.payload, lite3Frame.payload);

    const stream = await pool.fetchLite3({ since: seq2 });
    const { value: next } = await stream.next();
    assert.ok(next);
    assert.equal(next.seq, seq2);
    assert.deepEqual(next.payload, lite3Frame.payload);
    assert.equal((await stream.next()).done, true);
    stream.close();
  });
});

test("lite3 append rejects invalid payloads", async () => {
  await withPool("lite3-bad", async ({ pool }) => {
    await assert.rejects(() => pool.feedLite3Receipt(Buffer.from([0x01]), { durability: Durability.Fast }));
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
    for await (const message of await pool.follow({ since: "oldest",
      tags: ["keep", "prod"],
    })) {
      assert.equal(message.seq, 1n);
      assert.equal(typeof message.time, "bigint");
      assert.deepEqual(message.tags, ["keep", "prod"]);
      break;
    }

    assert.ok(capturedUrl, "expected fetch URL to be captured");
    assert.equal(capturedUrl.pathname, "/v2/pools/demo/follow");
    assert.deepEqual(capturedUrl.searchParams.getAll("tag"), ["keep", "prod"]);
  } finally {
    global.fetch = originalFetch;
  }
});

test("remote tail preserves terminal retention-gap errors", async () => {
  const originalFetch = global.fetch;
  global.fetch = async () => ({
    ok: true,
    status: 200,
    body: new ReadableStream({
      start(controller) {
        controller.enqueue(
          new TextEncoder().encode(
            '{"type":"error","error":{"kind":"RetentionGap","message":"retention gap","missing":7,"next_seq":18}}\n',
          ),
        );
        controller.close();
      },
    }),
  });

  try {
    const client = new RemoteClient("http://127.0.0.1:9700");
    const pool = new RemotePool(client, "demo");
    await assert.rejects(
      async () =>
        (await pool.follow({ since: 7 })).next(),
      (err) => {
        assert.ok(err instanceof RemoteError);
        assert.equal(err.kind, ErrorKind.RetentionGap);
        assert.equal(err.missing, 7n);
        assert.equal(err.nextSeq, 18n);
        return true;
      },
    );
  } finally {
    global.fetch = originalFetch;
  }
});

test("remote errors with unknown kind map to Io", async () => {
  const originalFetch = global.fetch;
  global.fetch = async () => ({
    ok: false,
    status: 400,
    async text() {
      return '{"error":{"kind":"NoSuchKind","message":"bad request"}}';
    },
  });

  try {
    const client = new RemoteClient("http://127.0.0.1:9700");
    const pool = new RemotePool(client, "demo");
    await assert.rejects(
      pool.feed({ value: 1 }),
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
      pool.feed({ value: 1 }, { durability: "bogus" }),
      /durability must be Durability\.Fast or Durability\.Flush/
    );
    assert.equal(called, false);
  } finally {
    global.fetch = originalFetch;
  }
});

test("UDP SDK waits without blocking, cancels, and rejects imprecise sequence numbers", async () => {
  let cancelCalls = 0;
  const follow = new UdpFollow({
    async nextEventWait(timeoutMs) {
      assert.equal(timeoutMs, 1000n);
      await new Promise(resolve => setTimeout(resolve, 10));
      return { status: 0, event: null };
    },
    get isFinished() {
      return false;
    },
    cancel() {
      cancelCalls += 1;
    },
    close() {},
  });
  const pool = new UdpPool({
    follow() {
      assert.fail("unsafe start values must be rejected before calling the native addon");
    },
    close() {},
  });

  await assert.rejects(
    pool.follow({ since: Number.MAX_SAFE_INTEGER + 1 }),
    err => err.kind === ErrorKind.Usage,
  );

  let timerFired = false;
  const timer = setTimeout(() => {
    timerFired = true;
    follow.cancel();
  }, 5);
  try {
    assert.equal(await follow.nextEvent(1), null);
    assert.equal(timerFired, true);
    assert.equal(cancelCalls, 1);
    assert.equal(follow.finished, true);
  } finally {
    clearTimeout(timer);
    follow.close();
    pool.close();
  }
});

test("UDP follow handshake is async, bounded, and safe when its pool closes", async () => {
  const blackhole = dgram.createSocket("udp4");
  const firstRequest = new Promise((resolve) => {
    blackhole.once("message", (message) => resolve(message));
  });
  await new Promise((resolve, reject) => {
    blackhole.once("error", reject);
    blackhole.bind(0, "127.0.0.1", resolve);
  });

  const port = blackhole.address().port;
  const client = new UdpClient(`udp://127.0.0.1:${port}`);
  const pool = client.openPool("udp-opening");
  let openingSettled = false;
  let closedAt = null;
  let openedFollow = null;
  const startedAt = Date.now();
  let closeTimer;
  let requestTimer;

  try {
    const closeDuringOpen = new Promise((resolve) => {
      closeTimer = setTimeout(() => {
        closedAt = Date.now();
        pool.close();
        resolve();
      }, 25);
    });
    const opening = pool.fetch({ since: "oldest" }).then(
      (follow) => {
        openingSettled = true;
        openedFollow = follow;
        return { follow };
      },
      (error) => {
        openingSettled = true;
        return { error };
      },
    );
    const requestTimeout = new Promise((_, reject) => {
      requestTimer = setTimeout(() => reject(new Error("UDP follow sent no request")), 1500);
    });
    const request = await Promise.race([firstRequest, requestTimeout]);
    clearTimeout(requestTimer);
    assert.ok(request.length > 0);
    await closeDuringOpen;

    assert.ok(closedAt - startedAt < 500, "pool close waited for the native handshake");
    assert.equal(openingSettled, false, "the silent peer should leave the handshake pending");

    const result = await opening;
    const elapsedMs = Date.now() - startedAt;
    assert.ok(result.error instanceof Error, "silent peer should fail the startup handshake");
    assert.match(result.error.message, /startup deadline/);
    assert.ok(elapsedMs >= 4500, `handshake ended before its deadline (${elapsedMs} ms)`);
    assert.ok(elapsedMs < 7000, `handshake exceeded its five-second bound (${elapsedMs} ms)`);
  } finally {
    clearTimeout(closeTimer);
    clearTimeout(requestTimer);
    openedFollow?.close();
    pool.close();
    client.close();
    await new Promise((resolve) => blackhole.close(resolve));
  }
});

test("UDP SDK sends JSON and exact Lite3 through a server process", async (t) => {
  const repoRoot = path.resolve(__dirname, "../../..");
  const binary = process.env.PLASMITE_BIN ?? path.join(repoRoot, "target", "debug", "plasmite");
  if (!fs.existsSync(binary)) {
    t.skip(`Plasmite CLI is not built: ${binary}`);
    return;
  }

  const temp = makeTempDir();
  const poolDir = path.join(temp, "pools");
  const home = path.join(temp, "home");
  const ready = path.join(temp, "server-ready");
  fs.mkdirSync(poolDir, { recursive: true });
  fs.mkdirSync(home, { recursive: true });

  const held = dgram.createSocket("udp4");
  const port = await new Promise((resolve, reject) => {
    held.once("error", reject);
    held.bind(0, "127.0.0.1", () => resolve(held.address().port));
  });
  await new Promise((resolve) => held.close(resolve));

  const localClient = new Client(poolDir);
  const localPool = localClient.createPool("udp-sdk", DEFAULT_POOL_SIZE_BYTES);
  const original = await feedForTest(localPool, { origin: "lite3", value: 7 }, ["source"]);
  const originalLite3 = localPool._getLite3(original.seq).payload;
  const server = childProcess.spawn(
    binary,
    [
      "--dir", poolDir,
      "serve",
      "--bind", "127.0.0.1:0",
      "--remote-bind", "127.0.0.1:0",
      `--udp=127.0.0.1:${port}`,
      "--udp-follow",
    ],
    {
      cwd: repoRoot,
      env: {
        ...process.env,
        HOME: home,
        PLASMITE_ACCESS_HOME: home,
        PLASMITE_SERVE_READY_FILE: ready,
      },
      stdio: ["ignore", "ignore", "pipe"],
    },
  );
  let serverError = "";
  server.stderr.setEncoding("utf8");
  server.stderr.on("data", (chunk) => { serverError += chunk; });

  let udpClient;
  let udpPool;
  let follow;
  try {
    const readyDeadline = Date.now() + 10_000;
    while (!fs.existsSync(ready)) {
      if (server.exitCode !== null) {
        assert.fail(`server exited before readiness: ${serverError}`);
      }
      if (Date.now() >= readyDeadline) {
        assert.fail(`server did not become ready: ${serverError}`);
      }
      await new Promise((resolve) => setTimeout(resolve, 20));
    }

    udpClient = new UdpClient(`udp://127.0.0.1:${port}`);
    udpPool = udpClient.openPool("udp-sdk");
    follow = await udpPool.follow({ since: original.seq + 1n });

    assert.equal(await udpPool.feed({ origin: "json", value: 8 }, { tags: ["sensor"] }), undefined);
    const jsonEvent = await nextUdpFrame(follow);
    assert.equal(jsonEvent.message.data.origin, "json");
    assert.equal(jsonEvent.message.data.value, 8);
    assert.deepEqual(jsonEvent.message.tags, ["sensor"]);
    const jsonTime = localPool._getLite3(jsonEvent.seq).time;
    assert.equal(jsonEvent.time, jsonTime);
    assert.ok(jsonEvent.time > (1n << 53n));
    assert.equal(parseMessage(localPool._getJson(jsonEvent.seq)).data.value, 8);

    assert.equal(await udpPool.feedLite3(originalLite3), undefined);
    const lite3Event = await nextUdpFrame(follow);
    assert.deepEqual(lite3Event.payload, originalLite3);
    assert.equal(lite3Event.message.data.origin, "lite3");
    assert.deepEqual(lite3Event.message.tags, ["source"]);
    assert.equal(parseMessage(localPool._getJson(lite3Event.seq)).data.value, 7);

    const largeStart = (1n << 53n) + 1n;
    await assert.rejects(udpPool.fetch({ since: largeStart }),
      err => err.kind === ErrorKind.Usage);

    const cancelFollow = await udpPool.follow();
    await cancelFollow.nextEvent(0);
    const controller = new AbortController();
    const pending = cancelFollow.nextEvent(5, { signal: controller.signal });
    setTimeout(() => controller.abort(), 10);
    await assert.rejects(pending, (err) => err.name === "AbortError");
    // Aborting one wait leaves the follower reusable and releases its read lease.
    assert.equal(await cancelFollow.nextEvent(0), null);
    cancelFollow.close();

    const ownedFollow = await udpPool.fetchView({ since: original.seq });
    let ownedFrame;
    for await (const event of ownedFollow) { if (event.kind === "frame") { ownedFrame = event; break; } }
    ownedFollow.close();
    assert.equal(ownedFrame.message.data.origin, "lite3");
    assert.equal(ownedFrame.message.seq, original.seq);
    assert.equal(ownedFrame.payload, undefined);
    assert.throws(() => { ownedFrame.message.data.value = 123; }, TypeError);

    const returnFollow = await udpPool.follow();
    await returnFollow.nextEvent(0);
    const events = returnFollow.events({ pollIntervalMs: 5000 });
    const idleNext = events.next();
    setTimeout(() => events.return(), 5);
    assert.equal((await idleNext).done, true);
    returnFollow.close();

    const closeFollow = await udpPool.follow();
    await closeFollow.nextEvent(0);
    const pendingClose = closeFollow.nextEvent(5);
    setTimeout(() => closeFollow.close(), 10);
    assert.equal(await pendingClose, null);
  } finally {
    if (follow) follow.close();
    if (udpPool) udpPool.close();
    if (udpClient) udpClient.close();
    localPool.close();
    localClient.close();
    if (server.exitCode === null) {
      server.kill("SIGTERM");
      await Promise.race([
        new Promise((resolve) => server.once("exit", resolve)),
        new Promise((resolve) => setTimeout(resolve, 3000)),
      ]);
    }
    if (server.exitCode === null) server.kill("SIGKILL");
    fs.rmSync(temp, { recursive: true, force: true });
  }
});

async function nextUdpFrame(follow) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const event = await follow.nextEvent(0.1);
    if (event?.kind === "frame") return event;
  }
  assert.fail("UDP follower did not receive a frame");
}

test("UDP native frames retain arrival order, duplicates, and uint64 metadata", { timeout: 5000 }, async (t) => {
  const temp = makeTempDir();
  const client = new Client(temp);
  const pool = client.createPool("udp-order", DEFAULT_POOL_SIZE_BYTES);
  const original = await feedForTest(pool, { value: 17 }, ["tag"]);
  const payload = pool._getLite3(original.seq).payload;
  const base = 1n << 53n;
  const timestamp = 1_700_000_000_000_000_001n;
  const peer = dgram.createSocket("udp4");
  let udpClient, udpPool, follow;
  t.after(() => {
    follow?.close();
    udpPool?.close();
    udpClient?.close();
    peer.close();
    pool.close();
    client.close();
    fs.rmSync(temp, { recursive: true, force: true });
  });
  await new Promise((resolve) => peer.bind(0, "127.0.0.1", resolve));
  let started = false;
  peer.on("message", (request, address) => {
    if (request[6] !== 1 || started) return;
    const id = request.subarray(8, 24);
    const subscribe = JSON.parse(request.subarray(24));
    const send = (op, body) => peer.send(Buffer.concat([
      Buffer.from([0, 80, 85, 68, 80, 1, op, 0]), id, Buffer.from(body),
    ]), address.port, address.address);
    if (!subscribe.cookie) {
      send(3, JSON.stringify({ cookie: { expires_ms: 10000, mac: "test-cookie" } }));
      return;
    }
    assert.equal(subscribe.cookie.mac, "test-cookie");
    started = true;
    const data = (seq) => {
      const header = Buffer.alloc(16);
      header.writeBigUInt64BE(seq);
      header.writeBigUInt64BE(timestamp, 8);
      send(7, Buffer.concat([header, payload]));
    };
    const progress = (terminal) => send(4,
      `{"start_seq":${base + 10n},"next_seq":${base + 13n},"end_seq":${base + 12n},"lease_ms":30000,"terminal":${terminal}}`);
    data(base + 10n);
    progress(false);
    send(5, `{"first":${base + 11n},"last":${base + 11n},"reason":"pressure"}`);
    for (const seq of [base + 12n, base + 11n, base + 12n]) data(seq);
    progress(true);
  });
  udpClient = new UdpClient(`udp://127.0.0.1:${peer.address().port}`);
  udpPool = udpClient.openPool("sensors");
  follow = await udpPool.fetch({ last: 4 });
  const events = [];
  for (let i = 0; i < 7; i++) events.push(await follow.nextEvent(1));
  assert.equal(await follow.nextEvent(0), null);
  const frames = events.filter((event) => event?.kind === "frame");
  assert.deepEqual(frames.map((frame) => frame.seq), [base + 10n, base + 12n, base + 11n, base + 12n]);
  for (const frame of frames) {
    assert.deepEqual(frame.payload, payload);
    assert.equal(frame.time, timestamp);
    assert.equal(frame.message.seq, frame.seq);
    assert.deepEqual(frame.message.data, { value: 17 });
  }
  assert.deepEqual(events.filter((event) => event?.kind === "skip").map((event) => [event.first, event.last, event.reason]),
    [[base + 11n, base + 11n, "pressure"]]);
  const progress = events.filter((event) => event?.kind === "progress");
  assert.deepEqual(progress.map((event) => event.nextSeq), [base + 13n, base + 13n]);
  assert.equal(progress.at(-1).terminal, true);
});


test("numeric feed receipts match stored frames without echoing data", async () => {
  await withPoolAsync("identities", async ({ pool }) => {
    const value = { nested: [1, { text: "hello" }] };
    const first = await pool.feedReceipt(value, { tags: ["tag"], durability: Durability.Flush });
    assert.deepEqual(Object.keys(first).sort(), ["seq", "time"]);
    assert.equal(first.seq, 1n);
    const stored = pool._getLite3(first.seq);
    assert.equal(first.time, stored.time);
    assert.ok(first.time > 2n ** 53n);
    value.nested.length = 0;
    assert.deepEqual(parseMessage(pool._getJson(first.seq)).data, { nested: [1, { text: "hello" }] });
    const second = await pool.feedLite3Receipt(stored.payload);
    assert.equal(second.seq, 2n);
    assert.equal(second.time, pool._getLite3(2n).time);
    await assert.rejects(() => pool.feedJsonReceipt(Buffer.from("bad")), PlasmiteNativeError);
    await assert.rejects(() => pool.feedLite3Receipt(Buffer.from("bad")), PlasmiteNativeError);
    assert.equal((await pool.feedReceipt({})).seq, 3n);
  });
});

test("follow defaults to new messages and history requires a selection", async () => {
  await withPoolAsync("follow-default", async ({pool}) => {
    await feedForTest(pool, {old:1}, ["skip"]);
    await feedForTest(pool, {old:2}, ["keep"]);
    const stream = (await pool.follow())._stream;
    try {
      assert.equal((await stream.nextJsonWait(0)).status, 0);
      const appended = await feedForTest(pool, {new:1}, ["keep"]);
      const item = await stream.nextJsonWait(100);
      assert.equal(item.status, 1);
      assert.equal(JSON.parse(item.message.toString()).seq, Number(appended.seq));
    } finally { stream.close(); }
    const seen=[];
    for await (const message of await pool.fetch({ last: 2 })) seen.push(message.seq);
    assert.deepEqual(seen,[2n,3n]);
    const views=[];
    for await (const message of await pool.fetchView()) views.push(message.seq);
    assert.deepEqual(views,[1n,2n,3n]);
  });
});


test("2.0 listener opens before first pull, resumes, and inspects history", async () => {
  await withPoolAsync("v2-listener", async ({ pool }) => {
    const old = await pool.feed({ n: 1 }, { tags: ["keep"] });
    const reader = await pool.follow({ tags: ["keep"] });
    try {
      assert.deepEqual(await reader.next(0), { done: false, value: undefined });
      await pool.feed({ n: 2 });
      const added = await pool.feed({ n: 3 }, { tags: ["keep"] });
      assert.equal((await reader.next()).value.seq, added.seq);
      assert.equal(typeof added.time, "bigint");
      assert.deepEqual(pool.info().bounds, { oldest: old.seq, newest: added.seq });
      const batch = await pool.fetch({ since: added.seq, tags: ["keep"] });
      const forwarded = await pool.feed(added);
      assert.deepEqual(forwarded.tags, added.tags);
      assert.equal((await batch.next()).value.seq, added.seq);
      assert.equal((await batch.next()).done, true);
      await assert.rejects(pool.feed(added, { tags: [] }), err => err.kind === ErrorKind.Usage);
      const view = await pool.feedView(added);
      assert.deepEqual([...view.meta.tags], added.tags);
      await assert.rejects(pool.feedView(view, { tags: [] }), err => err.kind === ErrorKind.Usage);
      await assert.rejects(pool.fetch({ since: "oldest", last: 1 }), err => err.kind === ErrorKind.Usage);
    } finally {
      await reader.return();
    }
  });
});

const conformance = JSON.parse(fs.readFileSync(
  path.resolve(__dirname, "../../../tests/fixtures/conformance/reads.json"), "utf8",
));

async function conformanceFeed(pool, records) {
  for (const record of records) await pool.feed(record.data, { tags: record.tags });
}

function selection({ since, last, tags }) {
  return { since, last, tags };
}

async function checkReadCases(pool) {
  for (const [single, cases] of [[false, conformance.fetch], [true, conformance.single_fetch]]) {
    for (const entry of cases) {
      const start = () => pool.fetch(selection(entry));
      if (entry.error) {
        await assert.rejects(start, err => err.kind === ErrorKind[entry.error], entry.name);
        continue;
      }
      const reader = await start();
      try {
        const sequences = [];
        for await (const message of reader) {
          sequences.push(Number(message.seq));
          if (single) break;
        }
        assert.deepEqual(sequences, entry.expect, entry.name);
      } finally {
        await reader.return();
      }
    }
  }

  const snapshot = conformance.fetch_snapshot;
  const reader = await pool.fetch();
  await conformanceFeed(pool, snapshot.after_open);
  const sequences = [];
  for await (const message of reader) sequences.push(Number(message.seq));
  assert.deepEqual(sequences, snapshot.expect, snapshot.name);
}

test("2.0 shared conformance covers retained history, snapshots, and gaps", async () => {
  await withPoolAsync("events", async ({ client, pool }) => {
    assert.throws(() => client.openPool("missing"), err => err.kind === ErrorKind[conformance.pools.missing]);
    assert.throws(() => client.createPool("events"), err => err.kind === ErrorKind[conformance.pools.existing]);
    await assert.rejects(pool.feed([]), err => err.kind === ErrorKind[conformance.pools.non_object_feed]);
    await conformanceFeed(pool, conformance.seed);
    await checkReadCases(pool);
  });
  const retained = conformance.retention;
  await withPoolAsync("retained", async ({ pool }) => {
    for (let i = 0; i < retained.inserts; i++) {
      await pool.feed({ i, padding: "x".repeat(retained.padding_bytes) });
    }
    await assert.rejects(pool.fetch({ since: retained.since, tags: retained.tags }), err => {
      assert.equal(err.kind, ErrorKind[retained.error]);
      assert.equal(err.missing, BigInt(retained.since));
      assert.equal(err.nextSeq, pool.info().bounds.oldest);
      return true;
    });
  }, retained.size);
  await withPoolAsync("unused", async ({ client, poolDir }) => {
    const previous = conformance.previous_release;
    fs.copyFileSync(path.resolve(__dirname, "../../..", previous.fixture),
      path.join(poolDir, "previous.plasmite"));
    const pool = client.openPool("previous");
    try {
      const records = [];
      for await (const message of await pool.fetch()) {
        records.push({ data: message.data, tags: message.tags });
      }
      assert.deepEqual(records, previous.messages);
    } finally {
      pool.close();
    }
  });
});

async function checkLiveCases(withFreshPool) {
  for (const entry of conformance.follow) {
    await withFreshPool(async pool => {
      await conformanceFeed(pool, conformance.seed);
      const controller = new AbortController();
      const reader = await pool.follow({ ...selection(entry), signal: controller.signal });
      try {
        await conformanceFeed(pool, entry.after_open ?? []);
        const sequences = [];
        for (const _ of entry.expect) sequences.push(Number((await reader.next(2)).value.seq));
        assert.deepEqual(sequences, entry.expect, entry.name);
        controller.abort();
        assert.equal((await reader.next()).done, true);
      } finally {
        await reader.return();
      }
    });
  }
}

test("2.0 shared conformance covers live starts, checkpoints, and cancellation", async () => {
  await checkLiveCases(run => withPoolAsync("events", ({ pool }) => run(pool)));
});

test("2.0 shared reads use the same contract over HTTP", { timeout: 20000 }, async t => {
  const binary = process.env.PLASMITE_BIN ?? path.resolve(__dirname, "../../../target/debug/plasmite");
  if (!fs.existsSync(binary)) {
    t.skip("Plasmite CLI is not built");
    return;
  }
  const directory = makeTempDir();
  const server = childProcess.spawn(binary, [
    "--dir", directory, "serve", "--bind", "127.0.0.1:0", "--remote-bind", "127.0.0.1:0",
  ], {
    env: { ...process.env, PLASMITE_ACCESS_HOME: path.join(directory, "access") },
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  server.stderr.on("data", chunk => { stderr += chunk; });
  t.after(async () => {
    if (server.exitCode === null) {
      const exited = new Promise(resolve => server.once("exit", resolve));
      server.kill();
      await exited;
    }
    fs.rmSync(directory, { recursive: true, force: true });
  });
  const bind = path.join(directory, ".plasmite-serve", "local.json");
  const deadline = Date.now() + 5000;
  while (!fs.existsSync(bind)) {
    assert.ok(server.exitCode === null && Date.now() < deadline, stderr || "server did not start");
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  const client = new RemoteClient("http://" + JSON.parse(fs.readFileSync(bind, "utf8")));
  assert.equal((await client.listPools()).length, 0);
  await assert.rejects(client.openPool("missing"), { kind: ErrorKind[conformance.pools.missing] });
  const history = await client.createPool("history");
  await assert.rejects(client.createPool("history"), { kind: ErrorKind[conformance.pools.existing] });
  await assert.rejects(history.feed([]), { kind: ErrorKind[conformance.pools.non_object_feed] });
  await conformanceFeed(history, conformance.seed);
  await checkReadCases(history);

  let next = 0;
  await checkLiveCases(async run => run(await client.createPool(`live-${next++}`)));

  const retained = conformance.retention;
  const pool = await client.createPool("retained", retained.size);
  for (let i = 0; i < retained.inserts; i++) {
    await pool.feed({ i, padding: "x".repeat(retained.padding_bytes) });
  }
  const reader = await pool.fetch({ since: retained.since, tags: retained.tags });
  const oldest = (await pool.info()).bounds.oldest;
  try {
    await assert.rejects(reader.next(), {
      kind: ErrorKind[retained.error], missing: BigInt(retained.since), nextSeq: oldest,
    });
  } finally {
    await reader.return();
  }
});
