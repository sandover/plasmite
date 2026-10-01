/* Exact sequence IDs at local and remote JSON boundaries. */
const test = require("node:test");
const assert = require("node:assert/strict");
const { Message, parseMessage, parseMessageJson } = require("../message");
const { RemoteClient, RemotePool } = require("../remote");

const time = "2026-01-01T00:00:00Z";
const sequences = ["9007199254740992", "9007199254740993", "18446744073709551615"];
const numericForms = [
  ["1e2", 100n],
  ["100.0", 100n],
  ["1000e-1", 100n],
  ["9.007199254740993e15", 9007199254740993n],
  ["184467440737095516150e-1", 18446744073709551615n],
  ["1.8446744073709551615e19", 18446744073709551615n],
  ["0e9999999999999999999999999", 0n],
  ["-0.0e-99999999999999999999999", 0n],
];

function envelope(seq) {
  return `{"seq":${seq},"time":"${time}","meta":{"tags":["exact"]},"data":{"seq":9007199254740993,"nested":{"message":{"seq":9007199254740993}}}}`;
}

function assertExact(message, seq) {
  assert.equal(message.seq, BigInt(seq));
  assert.equal(message.data.seq, Number("9007199254740993"));
  assert.equal(typeof message.data.seq, "number");
  assert.equal(typeof message.data.nested.message.seq, "number");
  assert.deepEqual(message.tags, ["exact"]);
}

test("local raw message parsing preserves all uint64 sequence digits", () => {
  for (const seq of sequences) {
    const raw = Buffer.from(envelope(seq));
    const message = parseMessage(raw);
    assertExact(message, seq);
    assert.strictEqual(message.raw, raw);
  }
});

test("raw JSON accepts exact integer decimal and scientific sequence forms", () => {
  for (const [source, seq] of numericForms) {
    assertExact(parseMessage(Buffer.from(envelope(source))), seq);
    const response = parseMessageJson(`{"message":${envelope(source)}}`, "message");
    assertExact(new Message(response.message), seq);
  }
});

test("response parsing changes only the selected message envelope sequence", () => {
  const response = parseMessageJson(
    `{"seq":9007199254740993,"message":${envelope("9007199254740993")}}`,
    "message",
  );
  assert.equal(typeof response.seq, "number");
  assertExact(new Message(response.message), "9007199254740993");
  assert.deepEqual(parseMessageJson('{"pools":[{"seq":9007199254740993}]}', "message"),
    { pools: [{ seq: Number("9007199254740993") }] });
});

test("remote append, fetch, and tail preserve exact uint64 message sequences", async () => {
  const originalFetch = global.fetch;
  try {
    for (const [source, seq] of [
      ...sequences.map((value) => [value, BigInt(value)]),
      ...numericForms,
    ]) {
      global.fetch = async (url) => new Response(
        new URL(url).pathname.endsWith("/tail")
          ? `${envelope(source)}\n`
          : `{"message":${envelope(source)}}`,
        { headers: { "Content-Type": "application/json" } },
      );
      const pool = new RemotePool(new RemoteClient("http://localhost:9700"), "exact");
      assertExact(await pool.append({ test: true }), seq);
      assertExact(await pool.get(BigInt(seq)), seq);
      const seen = [];
      for await (const message of pool.tail({ maxMessages: 1 })) {
        assertExact(message, seq);
        assert.equal(message.raw.toString(), envelope(source));
        seen.push(message.seq);
      }
      assert.deepEqual(seen, [BigInt(seq)]);
    }
  } finally {
    global.fetch = originalFetch;
  }
});

test("message object sequences reject unsafe, fractional, and invalid uint64 values", () => {
  for (const seq of [
    Number.MAX_SAFE_INTEGER + 1, 1.5, -1, NaN, Infinity,
    -1n, 1n << 64n, "18446744073709551616", "-1", "1.5", "", " ", "0x10",
  ]) {
    assert.throws(() => parseMessage({ seq, time, data: null }), { name: /^(TypeError|RangeError)$/ }, String(seq));
  }
  for (const seq of [0, Number.MAX_SAFE_INTEGER, 0n, "0", "18446744073709551615"]) {
    assert.equal(parseMessage({ seq, time, data: null }).seq, BigInt(seq));
  }
});

test("raw local and remote message sequences reject invalid uint64 boundaries", async () => {
  const originalFetch = global.fetch;
  try {
    for (const seq of [
      "-1", "1.5", "1e-1", "9007199254740991.1", "18446744073709551616",
      "1.8446744073709551616e19", "18446744073709551616.0",
      "1e9999999999999999999999999", "1e-9999999999999999999999999",
    ]) {
      assert.throws(() => parseMessage(Buffer.from(envelope(seq))));
      global.fetch = async (url) => new Response(
        new URL(url).pathname.endsWith("/tail")
          ? `${envelope(seq)}\n`
          : `{"message":${envelope(seq)}}`,
      );
      const pool = new RemotePool(new RemoteClient("http://localhost:9700"), "invalid");
      await assert.rejects(pool.append({}));
      await assert.rejects(pool.get(1));
      await assert.rejects(async () => {
        for await (const message of pool.tail()) {
          assert.fail(`invalid message ${message.seq} was accepted`);
        }
      });
    }
  } finally {
    global.fetch = originalFetch;
  }
});
