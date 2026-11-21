const test = require("node:test");
const assert = require("node:assert/strict");
const { RemoteClient, RemotePool, RemoteError } = require("../remote");

const { packet, frame, control } = require("./http_records");
const { encodeMessage } = require("../portable");
const { Message } = require("../views");

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
  const payload = encodeMessage({ data: Buffer.from([0, 255, 23, 0, 9]) });
  const seq = 9007199254740993n;
  const ns = 18446744073709551615n;
  const bytes = Buffer.concat([frame(seq, ns, payload), frame(seq + 1n, ns - 1n, payload), control({ type: "complete" })]);
  const chunks = [bytes.subarray(0, 3), bytes.subarray(3, 19), bytes.subarray(19, 23), bytes.subarray(23)];
  await stub(responseChunks(chunks), async (getCall) => {
    const pool = new RemotePool(new RemoteClient("https://example.com", { token: "secret" }), "a/b");
    const received = [];
    for await (const value of await pool.fetch({ since: seq, tags: ["x", "y"] })) received.push(value);
    assert.equal(received.length, 2);
    assert.equal(received[0].seq, seq);
    assert.ok(received[0] instanceof Message);
    assert.equal(received[0].time, ns);
    assert.equal(received[1].seq, seq + 1n);
    assert.deepEqual(received[0].raw, payload);
    assert.deepEqual(received[1].raw, payload);
    bytes.fill(0);
    assert.deepEqual(received[0].raw, payload);
    assert.deepEqual(received[1].raw, payload);
    const call = getCall();
    assert.match(call.url, /a%2Fb\/fetch/);
    assert.equal(new URL(call.url).searchParams.get("since"), String(seq));
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
      await assert.rejects(async () => { for await (const _ of await pool.follow()) {} }, /truncated/);
    });
  }
  const header = Buffer.alloc(5);
  header[0] = 0x4d;
  header.writeUInt32BE(256 * 1024 * 1024 + 17, 1);
  const response = responseChunks([header]);
  const original = Buffer.allocUnsafe;
  Buffer.allocUnsafe = (size) => {
    assert.ok(size < 1024 * 1024, "must reject the header before allocating its oversized payload");
    return original(size);
  };
  try {
    await stub(response, async () => {
      const pool = new RemotePool(new RemoteClient("http://localhost"), "p");
      await assert.rejects(async () => { for await (const _ of await pool.follow()) {} }, /invalid binary read record/);
    });
  } finally { Buffer.allocUnsafe = original; }
});

test("ending a binary tail cancels transport while retained payloads stay usable", async () => {
  let cancelled = false;
  const response = new Response(new ReadableStream({
    start(controller) { controller.enqueue(frame(1n, 1n, encodeMessage({ text: "retained" }))); },
    cancel() { cancelled = true; },
  }));
  await stub(response, async (getCall) => {
    const pool = new RemotePool(new RemoteClient("http://localhost"), "p");
    const iterator = await pool.follow();
    const first = await iterator.next();
    await iterator.return();
    assert.equal(cancelled, true);
    assert.equal(getCall().options.signal.aborted, true);
    assert.equal(first.value.data.text, "retained");
  });
});

test("binary appends preserve transport policy, durability and exact identity", async () => {
  const payload = encodeMessage({ checked: true });
  const response = new Response(payload, { headers: {
    "Content-Type": "application/x-plasmite-lite3",
    "Plasmite-Seq": "18446744073709551615",
    "Plasmite-Timestamp-Ns": "18446744073709551614",
  } });
  await stub(response, async (getCall) => {
    const pool = new RemotePool(new RemoteClient("https://example.com", { token: "secret" }), "p");
    await assert.rejects(pool.feed(payload, { format: "lite3", tags: ["override"] }), /cannot supply tags/);
    assert.equal(getCall(), undefined);
    assert.equal((await pool.feed(payload, { format: "lite3", durability: "flush" })).seq, 18446744073709551615n);
    assert.deepEqual(getCall().options.body, payload);
    assert.equal(getCall().options.headers["Content-Type"], "application/x-plasmite-lite3");
    assert.equal(new URL(getCall().url).searchParams.get("durability"), "flush");
    await assert.rejects(pool.feed(payload, { format: "lite3", durability: "constructor" }), /durability/);
  });
});

test("binary reads carry terminal retention errors and require a final record", async () => {
  const seq = 9007199254740993n;
  const failure = Buffer.from('{"type":"error","error":{"kind":"RetentionGap","message":"lost","missing":9007199254740993,"next_seq":9007199254741001}}');
  await stub(responseChunks([packet(0x43, failure)]), async () => {
    const pool = new RemotePool(new RemoteClient("http://localhost"), "p");
    await assert.rejects((await pool.follow({ since: seq })).next(), (error) => {
      assert.ok(error instanceof RemoteError);
      assert.equal(error.missing, seq);
      assert.equal(error.nextSeq, 9007199254741001n);
      assert.equal(error.kind, 9);
      return true;
    });
  });
  await stub(responseChunks([frame(1n, 1n, encodeMessage({}))]), async () => {
    const pool = new RemotePool(new RemoteClient("http://localhost"), "p");
    const reader = await pool.fetch();
    assert.equal((await reader.next()).value.seq, 1n);
    await assert.rejects(reader.next(), /without a control record/);
  });
});
