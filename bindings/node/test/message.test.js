/* Exact sequence IDs at local and remote JSON boundaries. */
const test = require("node:test");
const assert = require("node:assert/strict");
const { Message, parseMessage, parseMessageJson, parseNativeMessage } = require("../message");

const time = "2026-01-01T00:00:00Z";
const sequences = ["9007199254740992", "9007199254740993", "18446744073709551615"];
test("native parsing preserves exact identity, data numbers, and original bytes", () => {
  for (const seq of ["0", "42", ...sequences]) {
    const raw = Buffer.from(`{"seq":${seq},"time":"${time}","meta":{"tags":["native"]},"data":{"seq":9007199254740993,"nested":[{},[null,true]]}}`);
    const message = parseNativeMessage(raw);
    assert.deepEqual(message, parseMessage(raw));
    assert.strictEqual(message.raw, raw);
  }
  // A changed native field order must retain the exact general parser.
  const reordered = Buffer.from(`{"time":"${time}","seq":9007199254740993,"meta":{"tags":[]},"data":{}}`);
  assert.deepEqual(parseNativeMessage(reordered), parseMessage(reordered));
});

test("general parsing keeps rejecting fractions that round to safe integers", () => {
  for (const seq of ["1.00000000000000005", "-1e-9999", "9007199254740990.1"]) {
    const raw = Buffer.from(`{"seq":${seq},"time":"${time}","meta":{"tags":[]},"data":{}}`);
    assert.throws(() => parseMessage(raw));
    assert.throws(() => parseNativeMessage(raw));
  }
});
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

test("raw message sequences reject invalid uint64 boundaries", () => {
  for (const seq of [
    "-1", "1.5", "1e-1", "9007199254740991.1", "18446744073709551616",
    "1.8446744073709551616e19", "18446744073709551616.0",
    "1e9999999999999999999999999", "1e-9999999999999999999999999",
  ]) {
    assert.throws(() => parseMessage(Buffer.from(envelope(seq))));
  }
});
