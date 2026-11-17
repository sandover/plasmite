const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { Client, ErrorKind, toNative, toJson } = require('..');

test('native views retain exact values and independent snapshot lifetime', async t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-native-view-'));
  const client = new Client(dir);
  const pool = client.createPool('views', 65536);
  t.after(() => { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  pool.feedJson(Buffer.from('{"metrics":{"power":42},"samples":[{"value":1},{"value":2}],"large":9223372036854775807,"negativeZero":-0.0,"__proto__":{"x":1},"then":null,"mixed":[null,false,0,""],"�":5}'));
  const [first, second] = await Promise.all([pool._getView(1n), pool._getView(1n)]);
  assert.equal(first.seq, 1n);
  assert.equal('large' in first.data, true);
  assert.equal('absent' in first.data, false);
  assert.equal(first.data['\ud800'], undefined);
  assert.equal('\ud800' in first.data, false);
  assert.equal(first.data['�'], 5);
  assert.equal('bad\0key' in first.data, false);
  assert.equal(0 in first.data.samples, true);
  assert.equal(2 in first.data.samples, false);
  assert.deepEqual([...first.data.mixed], [null, false, 0, '']);
  assert.equal(typeof first.time, 'bigint');
  assert.equal(first.data.large, 9223372036854775807n);
  assert.equal(Object.is(first.data.negativeZero, -0), true);
  assert.equal(first.data.metrics, first.data.metrics);
  assert.deepEqual([...first.data.samples].map(x => x.value), [1, 2]);
  const native = toNative(first.data);
  assert.equal(Object.hasOwn(native, '__proto__'), true);
  assert.equal(Object.getPrototypeOf(native), Object.prototype);
  assert.equal(native.__proto__.x, 1);
  assert.match(toJson(first), /9223372036854775807/);
  assert.throws(() => JSON.stringify(first.data), TypeError);
  const metrics = first.data.metrics;
  const json = toJson(first.data);
  for (let i = 0; i < 30; i++) await pool.feed({ text: 'x'.repeat(4096) });
  await assert.rejects(pool._getView(1n), /not found|retention|sequence/i);
  pool.close();
  client.close();
  assert.equal(metrics.power, 42);
  assert.equal(second.data.samples.at(-1).value, 2);
  assert.equal(toJson(first.data), json);
  native.metrics.power = 1;
  assert.equal(first.data.metrics.power, 42);
});

test('queued reads retain their pool after close', async t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-native-view-'));
  const client = new Client(dir), pool = client.createPool('views', 65536);
  t.after(() => { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  await pool.feed({ x: 1 });
  const reads = [pool._getView(1n), pool._getView(1n)];
  pool.close();
  const views = await Promise.all(reads);
  assert.deepEqual(views.map(x => x.data.x), [1, 1]);
  await assert.rejects(pool._getView(1n), error => error.kind === ErrorKind.Usage && /closed/.test(error.message));
  assert.throws(() => pool._getJson(1n), error => error.kind === ErrorKind.Usage && /closed/.test(error.message));
});

test('native cursor supports early return and mixed values across chunks', async t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-native-cursor-'));
  const client = new Client(dir), pool = client.createPool('views', 1024 * 1024);
  t.after(() => { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  const values = Array.from({ length: 70 }, (_, i) => i % 3 === 0 ? null : i);
  await pool.feed({ values });
  const view = await pool._getView(1n);
  const first = view.data.values[Symbol.iterator]();
  assert.deepEqual(first.next(), { value: null, done: false });
  first.return();
  assert.deepEqual([...view.data.values], values);
  const retained = view.data.values[Symbol.iterator]();
  assert.equal(retained.next().value, null);
  pool.close();
  assert.deepEqual([...retained], values.slice(1));
});

test('native byte views preserve bytes and export independent mutable buffers', async t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-native-bytes-'));
  const client = new Client(dir), pool = client.createPool('views', 65536);
  t.after(() => { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  const token = Buffer.from('native-byte-fixture-with-zero');
  await pool.feed({ blob: token.toString() });
  const payload = pool._getLite3(1n).payload;
  // The vendored format shares length-prefixed string/byte storage, as the
  // core native-byte fixture does. Include the string terminator as a byte.
  const length = Buffer.alloc(4);
  length.writeUInt32LE(token.length + 1);
  const needle = Buffer.concat([Buffer.from([5]), length, token, Buffer.from([0])]);
  const position = payload.indexOf(needle);
  assert.ok(position >= 0);
  assert.equal(payload.indexOf(needle, position + 1), -1);
  payload[position] = 4; // LITE3_TYPE_BYTES, formerly LITE3_TYPE_STRING.
  const seq = pool.feedLite3Receipt(payload).seq;
  const view = await pool._getView(seq);
  const expected = Buffer.concat([token, Buffer.from([0])]);
  assert.equal(Array.isArray(view.data.blob), false);
  assert.deepEqual([...view.data.blob], [...expected]);
  const native = toNative(view.data);
  assert.ok(Buffer.isBuffer(native.blob));
  assert.deepEqual(native.blob, expected);
  native.blob[0] = 0;
  assert.equal(view.data.blob[0], token[0]);
  assert.throws(() => { view.data.blob[0] = 0; }, TypeError);
  assert.equal(toJson(view.data.blob), JSON.stringify(expected.toString('base64')));
});


test('native conversion preserves stored key order and exact scalar semantics', async t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-native-conversion-'));
  const client = new Client(dir), pool = client.createPool('views', 65536);
  t.after(() => { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  pool.feedJson(Buffer.from('{"z":-0.0,"9":9,"2":2,"a":"\\u0000\\n雪","safe":9007199254740991,"low":-9007199254740991,"nested":{"y":1,"b":2},"__proto__":{"ok":true}}'));
  const view = await pool._getView(1n);
  const native = toNative(view.data);
  assert.deepEqual(Object.keys(native), Object.keys(view.data));
  assert.deepEqual(Object.keys(native.nested), Object.keys(view.data.nested));
  assert.equal(Object.is(native.z, -0), true);
  assert.equal(native.a, '\0\n雪');
  assert.equal(native.safe, Number.MAX_SAFE_INTEGER);
  assert.equal(native.low, Number.MIN_SAFE_INTEGER);
  assert.equal(Object.hasOwn(native, '__proto__'), true);
  assert.equal(Object.getPrototypeOf(native), Object.prototype);
  pool.feedJson(Buffer.from('{"z":9223372036854775807,"a":-9223372036854775808}'));
  const unsafe = toNative((await pool._getView(2n)).data);
  assert.equal(unsafe.z, 9223372036854775807n);
  assert.equal(unsafe.a, -9223372036854775808n);
  assert.deepEqual(Object.getOwnPropertyDescriptor(unsafe, 'z'), {
    value: 9223372036854775807n, writable: true, enumerable: true, configurable: true,
  });
  unsafe.z = 1n;
  assert.equal(unsafe.z, 1n);
  assert.equal(delete unsafe.a, true);
});


test('native copies select the parser only for JSON-safe owners', async t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-native-json-safe-'));
  const client = new Client(dir), pool = client.createPool('views', 65536);
  t.after(() => { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  await pool.feed({ z: 1, a: 2 });
  pool.feedJson(Buffer.from('{"z":9223372036854775807}'));
  let parses = 0;
  const parse = text => { parses++; return JSON.parse(text); };
  const safe = await pool._inner.getView(1n);
  const safeData = safe.lookup(safe.root().node, 'data');
  assert.deepEqual(safe.materialize(safeData.node, parse), { z: 1, a: 2 });
  assert.equal(parses, 1);
  const unsafe = await pool._inner.getView(2n);
  const unsafeData = unsafe.lookup(unsafe.root().node, 'data');
  assert.equal(unsafe.materialize(unsafeData.node, parse).z, 9223372036854775807n);
  assert.equal(parses, 1);
});
