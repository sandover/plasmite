const test = require("node:test");
const assert = require("node:assert/strict");
const { RemoteClient, RemotePool, RemoteError } = require("../remote");

function frame(seq, timestampNs, payload) {
  const header = Buffer.alloc(20);
  header.writeBigUInt64BE(seq, 0);
  header.writeBigUInt64BE(timestampNs, 8);
  header.writeUInt32BE(payload.length, 16);
  return Buffer.concat([header, payload]);
}

async function stub(response, run) {
  const previous = global.fetch;
  let call;
  global.fetch = async (url, options) => { call = { url, options }; return response; };
  try { await run(() => call); } finally { global.fetch = previous; }
}

function responseChunks(chunks, cancelled = () => {}) {
  return new Response(new ReadableStream({
    start(controller) { for (const chunk of chunks) controller.enqueue(chunk); controller.close(); },
    cancel: cancelled,
  }));
}

test("binary HTTP tail preserves complete frames across arbitrary transport boundaries", async () => {
  const payload = Buffer.from([0, 255, 23, 0, 9]);
  const seq = 9007199254740993n;
  const ns = 18446744073709551615n;
  const bytes = Buffer.concat([frame(seq, ns, payload), frame(seq + 1n, ns - 1n, payload)]);
  const chunks = [bytes.subarray(0, 3), bytes.subarray(3, 19), bytes.subarray(19, 23), bytes.subarray(23)];
  await stub(responseChunks(chunks), async (getCall) => {
    const pool = new RemotePool(new RemoteClient("https://example.com", { token: "secret" }), "a/b");
    const received = [];
    for await (const value of pool.tailLite3({ sinceSeq: seq, maxMessages: 2, tags: ["x", "y"] })) received.push(value);
    assert.equal(received.length, 2);
    assert.equal(received[0].seq, seq);
    assert.equal(received[0].timestampNs, ns);
    assert.equal(received[1].seq, seq + 1n);
    assert.deepEqual(received[0].payload, payload);
    assert.deepEqual(received[1].payload, payload);
    assert.equal(received[0].time.getTime(), Number(ns / 1000000n));
    received[0].time.setTime(0);
    assert.equal(received[0].time.getTime(), Number(ns / 1000000n));
    bytes.fill(0);
    assert.deepEqual(received[0].payload, payload);
    assert.deepEqual(received[1].payload, payload);
    const call = getCall();
    assert.match(call.url, /a%2Fb\/tail_lite3/);
    assert.equal(new URL(call.url).searchParams.get("since_seq"), String(seq));
    assert.deepEqual(new URL(call.url).searchParams.getAll("tag"), ["x", "y"]);
    assert.equal(call.options.headers.Accept, "application/x-plasmite-lite3-stream");
    assert.equal(call.options.headers.Authorization, "Bearer secret");
    assert.equal(call.options.redirect, "manual");
  });
});

test("binary HTTP tails reject truncation and oversized frames before payload allocation", async () => {
  for (const bytes of [Buffer.alloc(3), frame(1n, 1n, Buffer.from("ab")).subarray(0, 21)]) {
    await stub(responseChunks([bytes]), async () => {
      const pool = new RemotePool(new RemoteClient("http://localhost"), "p");
      await assert.rejects(async () => { for await (const _ of pool.tailLite3()) {} }, /truncated/);
    });
  }
  const header = Buffer.alloc(20);
  header.writeUInt32BE(256 * 1024 * 1024 + 1, 16);
  const response = responseChunks([header]);
  const original = Buffer.allocUnsafe;
  Buffer.allocUnsafe = (size) => {
    assert.ok(size < 1024 * 1024, "must reject the header before allocating its oversized payload");
    return original(size);
  };
  try {
    await stub(response, async () => {
      const pool = new RemotePool(new RemoteClient("http://localhost"), "p");
      await assert.rejects(async () => { for await (const _ of pool.tailLite3()) {} }, /maximum/);
    });
  } finally { Buffer.allocUnsafe = original; }
});

test("ending a binary tail cancels transport while retained payloads stay usable", async () => {
  let cancelled = false;
  const response = new Response(new ReadableStream({
    start(controller) { controller.enqueue(frame(1n, 1n, Buffer.from("retained"))); },
    cancel() { cancelled = true; },
  }));
  await stub(response, async (getCall) => {
    const pool = new RemotePool(new RemoteClient("http://localhost"), "p");
    const iterator = pool.tailLite3();
    const first = await iterator.next();
    await iterator.return();
    assert.equal(cancelled, true);
    assert.equal(getCall().options.signal.aborted, true);
    assert.equal(first.value.payload.toString(), "retained");
  });
});

test("binary point reads require matching exact identity headers", async () => {
  const seq = 9007199254740993n;
  await stub(new Response(Buffer.from("payload"), { headers: {
    "plasmite-seq": String(seq), "plasmite-timestamp-ns": "123456789",
  } }), async () => {
    const pool = new RemotePool(new RemoteClient("http://localhost"), "p");
    const received = await pool.getLite3(seq);
    assert.equal(received.seq, seq);
    assert.equal(received.timestampNs, 123456789n);
    assert.equal(received.payload.toString(), "payload");
  });
  for (const headers of [{}, { "plasmite-seq": "1", "plasmite-timestamp-ns": "2" },
    { "plasmite-seq": String(seq), "plasmite-timestamp-ns": "1.5" }]) {
    await stub(new Response(Buffer.from("payload"), { headers }), async () => {
      const pool = new RemotePool(new RemoteClient("http://localhost"), "p");
      await assert.rejects(pool.getLite3(seq), /header|match/);
    });
  }
});

test("binary appends preserve transport policy, durability and exact receipt identity", async () => {
  const response = new Response('{"message":{"seq":18446744073709551615,"time":"2026-01-01T00:00:00Z","meta":{"tags":[]},"data":{}}}');
  await stub(response, async (getCall) => {
    const pool = new RemotePool(new RemoteClient("https://example.com", { token: "secret" }), "p");
    const payload = Buffer.from([1, 2, 3]);
    assert.equal(await pool.appendLite3(payload, "flush"), 18446744073709551615n);
    assert.equal(getCall().options.body, payload);
    assert.equal(getCall().options.headers["Content-Type"], "application/x-plasmite-lite3");
    assert.equal(new URL(getCall().url).searchParams.get("durability"), "flush");
    await assert.rejects(pool.appendLite3(payload, "constructor"), /durability/);
  });
});

test("binary requests keep structured HTTP errors and reject unsupported gap policy", async () => {
  await stub(new Response('{"error":{"kind":"NotFound","message":"missing","seq":9007199254740993}}', { status: 404 }), async () => {
    const pool = new RemotePool(new RemoteClient("http://localhost"), "p");
    await assert.rejects(pool.getLite3(9007199254740993n), (error) => {
      assert.ok(error instanceof RemoteError);
      assert.equal(error.seq, 9007199254740993n);
      assert.equal(error.kind, 3);
      return true;
    });
    await assert.rejects(async () => { for await (const _ of pool.tailLite3({ errorOnGap: true })) {} }, /retention gaps/);
  });
});
