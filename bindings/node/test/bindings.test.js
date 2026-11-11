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
  UdpStart,
  RemoteClient,
  RemoteError,
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
    fs.rmSync(temp, { recursive: true, force: true });
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

function fillPast(pool, seq) {
  for (let i = 0; i < 10000; i += 1) {
    pool.append({ padding: "x".repeat(4096), i });
    try {
      pool.get(seq);
    } catch (err) {
      if (err instanceof PlasmiteNativeError && err.kind === ErrorKind.NotFound) {
        return;
      }
      throw err;
    }
  }
  assert.fail(`sequence ${seq} remained retained after filling the pool`);
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
    fs.rmSync(temp, { recursive: true, force: true });
  }
}

test("append/get supports large payload and tags", () => {
  withPool("big", ({ pool }) => {
    const payload = { blob: "0123456789abcdef".repeat(4096) };
    const tags = ["alpha", "beta", "gamma"];
    const messageBuf = pool.appendJson(
      Buffer.from(JSON.stringify(payload)),
      tags,
      Durability.Fast
    );
    const message = JSON.parse(messageBuf.toString("utf8"));
    assert.deepEqual(message.data, payload);
    assert.deepEqual(message.meta.tags, tags);

    const fetchedBuf = pool.getJson(BigInt(message.seq));
    const fetched = JSON.parse(fetchedBuf.toString("utf8"));
    assert.deepEqual(fetched.data, payload);
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

test("tail filters by tags before limiting matching messages", async () => {
  await withPoolAsync("tail-matching", async ({ pool }) => {
    pool.append({ kind: "drop", i: 1 }, ["drop"]);
    pool.append({ kind: "keep", i: 2 }, ["keep"]);
    pool.append({ kind: "drop", i: 3 }, ["drop"]);
    pool.append({ kind: "keep", i: 4 }, ["keep"]);

    const tailed = [];
    for await (const message of pool.tail({ tags: ["keep"], maxMessages: 2, timeoutMs: 10 })) {
      tailed.push(message.data.i);
    }
    assert.deepEqual(tailed, [2, 4]);


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

test("a quiet native tail leaves timers free to append and abort", async () => {
  await withPoolAsync("responsive-tail", async ({ pool }) => {
    let timer;
    try {
      timer = setTimeout(() => pool.append({ from: "timer" }), 10);
      const result = await pool.tail({ maxMessages: 1, timeoutMs: 1000 }).next();
      assert.equal(result.done, false);
      assert.equal(result.value.data.from, "timer");
    } finally {
      clearTimeout(timer);
    }
    const controller = new AbortController();
    const tail = pool.tail({ sinceSeq: 2n, timeoutMs: 5000, signal: controller.signal });
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
    const stream = pool.openStream(1n, 1n);
    try {
      assert.equal((await stream.nextJsonWait(0)).status, 0);
      const message = pool.append({ value: 7 });
      const read = await stream.nextJsonWait(0);
      assert.equal(read.status, 1);
      assert.equal(parseMessage(read.message).seq, message.seq);
      assert.equal((await stream.nextJsonWait(0)).status, 2);
      const raw = pool.openLite3Stream(1n, 1n);
      try {
        const result = await raw.nextWait(0);
        assert.equal(result.status, 1);
        assert.equal(result.frame.seq, message.seq);
        assert.ok(result.frame.time instanceof Date);
        assert.equal((await raw.nextWait(0)).status, 2);
      } finally { raw.close(); }
    } finally { stream.close(); }
    for (const [stream, wait] of [
      [pool.openStream(2n), (s) => s.nextJsonWait(5000)],
      [pool.openLite3Stream(2n), (s) => s.nextWait(5000)],
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
    pool.append({ last: true });
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
    for await (const message of pool.tail({ sinceSeq: last, timeoutMs: 100 })) messages.push(message.seq);
    assert.deepEqual(messages, [last]);
  });
});

test("local tails can fail closed on retention gaps", async () => {
  await withPoolAsync(
    "tail-gap",
    async ({ pool }) => {
      const first = pool.append({ kind: "first" });
      fillPast(pool, first.seq);

      const continuedIterator = pool
        .tail({ sinceSeq: first.seq, maxMessages: 1, timeoutMs: 50 })
        [Symbol.asyncIterator]();
      const continued = await continuedIterator.next();
      assert.equal(continued.done, false);
      assert.ok(continued.value.seq > first.seq);
      await continuedIterator.return();

      await assert.rejects(
        async () =>
          pool
            .tail({
              sinceSeq: first.seq,
              tags: ["never"],
              timeoutMs: 50,
              errorOnGap: true,
            })
            [Symbol.asyncIterator]()
            .next(),
        (err) => {
          assert.ok(err instanceof PlasmiteNativeError);
          assert.equal(err.kind, ErrorKind.RetentionGap);
          assert.equal(err.seq, Number(first.seq));
          return true;
        },
      );
    },
    64 * 1024,
  );
});

test("an open stream reports the first unread sequence after overwrite", () => {
  withPool("slow-reader", ({ pool }) => {
    const seed = pool.append({ kind: "seed" });
    const stream = pool.openStream(seed.seq, null, 500n, true);
    try {
      assert.equal(parseMessage(stream.nextJson()).seq, seed.seq);
      const lost = pool.append({ kind: "lost" });
      fillPast(pool, lost.seq);
      assert.throws(() => stream.nextJson(), {
        kind: ErrorKind.RetentionGap,
        seq: Number(lost.seq),
      });
      assert.equal(stream.nextJson(), null);
    } finally {
      stream.close();
    }
  }, 64 * 1024);
});

test("lite3 streams can fail closed on retention gaps", () => {
  withPool(
    "lite3-gap",
    ({ pool }) => {
      const first = pool.append({ kind: "first" });
      fillPast(pool, first.seq);

      const stream = pool.openLite3Stream(first.seq, null, BigInt(50), true);
      assert.throws(
        () => stream.next(),
        (err) => {
          assert.ok(err instanceof PlasmiteNativeError);
          assert.equal(err.kind, ErrorKind.RetentionGap);
          assert.equal(err.seq, Number(first.seq));
          return true;
        },
      );
      stream.close();
    },
    64 * 1024,
  );
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

test("native error paths do not become error metadata", () => {
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

test("missing sequences and corrupt pool files carry different error details", () => {
  withPool("damaged", ({ client, pool, poolDir }) => {
    assert.throws(() => pool.get(42n), {
      kind: ErrorKind.NotFound,
      seq: 42,
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

test("native errors with unknown kind default to Io", () => {
  const pool = new Pool({
    appendJson() {
      throw Object.assign(new Error("plasmite error: kind=NoSuchKind; message=bad"), { kind: "NoSuchKind" });
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
      errorOnGap: true,
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
    assert.equal(capturedUrl.searchParams.get("gap_policy"), "error");
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
            '{"error":{"kind":"RetentionGap","message":"retention gap","seq":7}}\n',
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
        pool
          .tail({ sinceSeq: 7, errorOnGap: true })
          [Symbol.asyncIterator]()
          .next(),
      (err) => {
        assert.ok(err instanceof RemoteError);
        assert.equal(err.kind, ErrorKind.RetentionGap);
        assert.equal(err.seq, 7);
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

test("UDP SDK polls without blocking, cancels, and rejects imprecise sequence numbers", async () => {
  let cancelCalls = 0;
  const follow = new UdpFollow({
    nextEvent(timeoutMs) {
      assert.equal(timeoutMs, 0n);
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

  assert.throws(() => UdpStart.sequence(Number.MAX_SAFE_INTEGER + 1), TypeError);
  assert.throws(
    () => pool.follow({ start: { kind: "sequence", seq: Number.MAX_SAFE_INTEGER + 1 } }),
    TypeError,
  );

  let timerFired = false;
  const timer = setTimeout(() => {
    timerFired = true;
    follow.cancel();
  }, 5);
  try {
    assert.equal(await follow.nextEvent(1000), null);
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
    const opening = pool.follow({ live: false }).then(
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
  const original = localPool.append({ origin: "lite3", value: 7 }, ["source"]);
  const originalLite3 = localPool.getLite3(original.seq).payload;
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
    follow = await udpPool.follow({ start: UdpStart.sequence(original.seq + 1n), live: true });

    assert.equal(udpPool.sendJson({ origin: "json", value: 8 }, ["sensor"]), undefined);
    const jsonEvent = await nextUdpFrame(follow);
    assert.equal(jsonEvent.message.data.origin, "json");
    assert.equal(jsonEvent.message.data.value, 8);
    assert.deepEqual(jsonEvent.message.tags, ["sensor"]);
    const jsonTime = localPool.getLite3(jsonEvent.seq).timestampNs;
    assert.equal(jsonEvent.timestampNs, jsonTime);
    assert.ok(jsonEvent.timestampNs > (1n << 53n));
    assert.equal(localPool.get(jsonEvent.seq).data.value, 8);

    assert.equal(udpPool.sendLite3(originalLite3), undefined);
    const lite3Event = await nextUdpFrame(follow);
    assert.deepEqual(lite3Event.payload, originalLite3);
    assert.equal(lite3Event.message.data.origin, "lite3");
    assert.deepEqual(lite3Event.message.tags, ["source"]);
    assert.equal(localPool.get(lite3Event.seq).data.value, 7);

    const largeStart = (1n << 53n) + 1n;
    const largeFollow = await udpPool.follow({ start: UdpStart.sequence(largeStart), live: false });
    const largeProgress = await largeFollow.nextEvent(1000);
    assert.equal(largeProgress.startSeq, largeStart);
    largeFollow.close();

    const cancelFollow = await udpPool.follow();
    await cancelFollow.nextEvent(0n);
    const controller = new AbortController();
    const pending = cancelFollow.nextEvent(5000n, { signal: controller.signal });
    setTimeout(() => controller.abort(), 10);
    await assert.rejects(pending, (err) => err.name === "AbortError");
    cancelFollow.close();

    const closeFollow = await udpPool.follow();
    await closeFollow.nextEvent(0n);
    const pendingClose = closeFollow.nextEvent(5000n);
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
    const event = await follow.nextEvent(100);
    if (event?.kind === "frame") return event;
  }
  assert.fail("UDP follower did not receive a frame");
}

test("UDP native frames retain arrival order, duplicates, and uint64 metadata", { timeout: 5000 }, async (t) => {
  const temp = makeTempDir();
  const client = new Client(temp);
  const pool = client.createPool("udp-order", DEFAULT_POOL_SIZE_BYTES);
  const original = pool.append({ value: 17 }, ["tag"]);
  const payload = pool.getLite3(original.seq).payload;
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
  follow = await udpPool.follow({ start: UdpStart.last(4), live: false });
  const events = [];
  for (let i = 0; i < 7; i++) events.push(await follow.nextEvent(1000));
  assert.equal(await follow.nextEvent(0), null);
  const frames = events.filter((event) => event?.kind === "frame");
  assert.deepEqual(frames.map((frame) => frame.seq), [base + 10n, base + 12n, base + 11n, base + 12n]);
  for (const frame of frames) {
    assert.deepEqual(frame.payload, payload);
    assert.equal(frame.timestampNs, timestamp);
    assert.equal(frame.message.seq, frame.seq);
    assert.deepEqual(frame.message.data, { value: 17 });
  }
  assert.deepEqual(events.filter((event) => event?.kind === "gap").map((event) => [event.first, event.last, event.reason]),
    [[base + 11n, base + 11n, "pressure"]]);
  const progress = events.filter((event) => event?.kind === "progress");
  assert.deepEqual(progress.map((event) => event.nextSeq), [base + 13n, base + 13n]);
  assert.equal(progress.at(-1).terminal, true);
});
