const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { Client, ErrorKind, toNative, toJson } = require('..');
const { valueReference, createNativeMessage } = require('../views');

function fixture(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-node-input-'));
  const client = new Client(dir), pool = client.createPool('input', 1024 * 1024);
  t.after(() => { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  return { client, pool };
}

test('native input keeps exact numeric values, bytes and a detached receipt', async t => {
  const { pool } = fixture(t);
  const input = { integer: 42, smallBig: 42n, maximum: 9223372036854775807n,
    minimum: -9223372036854775808n, binary64: 9007199254740992,
    zero: -0, text: '雪😀�\0x', buffer: Buffer.from([0, 255]), bytes: new Uint8Array([1, 2]) };
  const pending = pool.appendView(input, [' z ', 'a', 'a', '\0']);
  input.integer = 0; input.buffer[0] = 9; input.bytes[0] = 9;
  const view = await pending;
  assert.equal(view.seq, 1n);
  assert.equal(typeof view.timestampNs, 'bigint');
  assert.equal(view.data.integer, 42);
  assert.equal(view.data.smallBig, 42);
  assert.equal(view.data.maximum, 9223372036854775807n);
  assert.equal(view.data.minimum, -9223372036854775808n);
  assert.equal(view.data.binary64, 9007199254740992);
  assert.equal(typeof view.data.binary64, 'number');
  assert.equal(Object.is(view.data.zero, -0), true);
  assert.equal(view.data.text, '雪😀�\0x');
  assert.deepEqual([...view.data.buffer], [0, 255]);
  assert.deepEqual([...view.data.bytes], [1, 2]);
  assert.deepEqual([...view.meta.tags], [' z ', 'a', 'a', '\0']);
  assert.equal(toJson(view.data.buffer), '"AP8="');
});

test('native input preserves JSON hook, Proxy and array access order', async t => {
  const { pool } = fixture(t);
  function input(log) {
    const array = new Proxy([1, , undefined, () => {}, Symbol('x')], {
      get(target, key, receiver) { log.push('array.get:' + String(key)); return Reflect.get(target, key, receiver); },
    });
    return new Proxy({
      get x() { log.push('getter:x'); return 2; },
      nested: { toJSON(key) { log.push('toJSON:' + key); return { value: 3 }; } },
      array, absent: undefined, fn() {}, symbol: Symbol('x'),
    }, {
      get(target, key, receiver) { log.push('get:' + String(key)); return Reflect.get(target, key, receiver); },
      ownKeys(target) { log.push('ownKeys'); return Reflect.ownKeys(target); },
      getOwnPropertyDescriptor(target, key) { log.push('descriptor:' + String(key)); return Reflect.getOwnPropertyDescriptor(target, key); },
    });
  }
  const expectedLog = [], actualLog = [];
  const expected = JSON.parse(JSON.stringify(input(expectedLog)));
  const view = await pool.appendView(input(actualLog));
  assert.deepEqual(toNative(view.data), expected);
  assert.deepEqual(actualLog, expectedLog);
});

test('native input respects boxed values and custom binary serializers', async t => {
  const { pool } = fixture(t);
  const buffer = Buffer.from([1, 2]);
  buffer.toJSON = key => ({ custom: key });
  class Bytes extends Uint8Array { toJSON(key) { return { subclass: key }; } }
  const boolean = new Boolean(true);
  boolean.valueOf = () => { throw new Error('must use internal boolean value'); };
  const input = { number: new Number(3), string: new String('text'), boolean,
    bigint: Object(7n), buffer, subclass: new Bytes([3]), date: new Date(0) };
  const view = await pool.appendView(input);
  assert.deepEqual(toNative(view.data), { number: 3, string: 'text', boolean: true, bigint: 7,
    buffer: { custom: 'buffer' }, subclass: { subclass: 'subclass' }, date: new Date(0).toJSON() });
  const invalid = new Number(1);
  invalid[Symbol.toPrimitive] = () => 1n;
  await assert.rejects(pool.appendView({ invalid }), TypeError);
});

test('native input preserves thrown values and rejects before publication', async t => {
  const { pool } = fixture(t);
  const sentinel = Object.assign(new Error('from getter'), { kind: 'Usage' });
  await assert.rejects(pool.appendView({ get value() { throw sentinel; } }), error => error === sentinel);
  const cycle = {}; cycle.self = cycle;
  for (const value of [cycle, { value: 9223372036854775808n }, { value: Infinity },
    { value: '\ud800' }, { '\udfff': 1 }, [], 1, Buffer.from([1])]) {
    await assert.rejects(pool.appendView(value), TypeError);
  }
  assert.equal((await pool.appendView({ good: '�' })).seq, 1n);
});

test('native input invokes getters once across writer growth', async t => {
  const { pool } = fixture(t);
  let calls = 0;
  const input = { get value() { calls++; return 'x'.repeat(65536); } };
  const view = await pool.appendView(input);
  assert.equal(calls, 1);
  assert.equal(view.data.value.length, 65536);
  const root = function () {};
  root.toJSON = key => ({ key });
  assert.equal((await pool.appendView(root)).data.key, '');
});

test('native input retains its private target when hooks close the pool', async t => {
  for (const mode of ['getter', 'toJSON']) {
    await t.test(mode, async t => {
      const { pool, client } = fixture(t);
      const input = mode === 'getter'
        ? { get value() { pool.close(); client.close(); return 42; } }
        : { toJSON() { pool.close(); client.close(); return { value: 42 }; } };
      const view = await pool.appendView(input);
      assert.equal(view.data.value, 42);
      await assert.rejects(pool.getView(view.seq), error => error.kind === ErrorKind.Usage);
    });
  }
});

test('queued native appends finish after close and retain their committed snapshots', async t => {
  const { pool, client } = fixture(t);
  const pending = [1, 2, 3].map(value => pool.appendView({ value }));
  pool.close(); client.close();
  const views = await Promise.all(pending);
  assert.deepEqual(views.map(view => view.data.value), [1, 2, 3]);
  assert.equal(new Set(views.map(view => view.seq)).size, 3);
});

test('native input copies checked subtrees without losing native bytes', async t => {
  const { pool } = fixture(t);
  const first = await pool.appendView({ nested: { value: 3n }, bytes: Buffer.from([0, 255]) });
  const copied = await pool.appendView({ subtree: first.data.nested, bytes: first.data.bytes });
  assert.equal(copied.data.subtree.value, 3);
  assert.deepEqual([...copied.data.bytes], [0, 255]);
  const root = await pool.appendView(first.data, ['retagged']);
  assert.equal(root.data.nested.value, 3);
  assert.deepEqual([...root.data.bytes], [0, 255]);
  assert.deepEqual([...root.meta.tags], ['retagged']);
});

test('whole-message forwarding preserves payload and optional retagging copies data', async t => {
  const { pool } = fixture(t);
  const first = await pool.appendView({ value: 9223372036854775807n, bytes: Buffer.from([7, 8]) }, ['original']);
  const forwarded = await pool.appendView(first);
  const retagged = await pool.appendView(first, ['changed']);
  assert.equal(forwarded.seq, 2n);
  assert.equal(retagged.seq, 3n);
  assert.equal(forwarded.data.value, 9223372036854775807n);
  assert.deepEqual([...forwarded.data.bytes], [7, 8]);
  assert.deepEqual([...forwarded.meta.tags], ['original']);
  assert.deepEqual([...retagged.meta.tags], ['changed']);
  assert.deepEqual([...retagged.data.bytes], [7, 8]);
  const ordinary = await pool.appendView({ seq: 99, meta: { tags: ['data'] }, data: { value: 1 } });
  assert.equal(ordinary.data.seq, 99);
  assert.equal(ordinary.data.data.value, 1);
});

test('forwarded children survive GC of original and intermediate wrappers', { skip: !global.gc }, async t => {
  const { pool, client } = fixture(t);
  let original = await pool.appendView({ bytes: Buffer.alloc(65536, 7) });
  let intermediate = await pool.appendView(original);
  let last = await pool.appendView(intermediate);
  const child = last.data.bytes;
  original = null; intermediate = null; last = null;
  pool.close(); client.close();
  for (let i = 0; i < 5; i++) {
    await new Promise(resolve => setImmediate(resolve));
    global.gc();
  }
  assert.equal(child.length, 65536);
  assert.equal(child[0], 7);
  assert.equal(child[65535], 7);
});

test('encoding failure releases its target after the hook closes the pool', async t => {
  const { pool, client } = fixture(t);
  const sentinel = new Error('encoding stopped');
  await assert.rejects(pool.appendView({ get value() {
    pool.close(); client.close(); throw sentinel;
  } }), error => error === sentinel);
  await assert.rejects(pool.appendView({}), error => error.kind === ErrorKind.Usage);
});

test('tag callbacks run after retaining the target and failures discard it', async t => {
  for (const fail of [false, true]) {
    await t.test(fail ? 'failure' : 'success', async t => {
      const { pool, client } = fixture(t);
      const sentinel = new Error('tag getter failed');
      let calls = 0;
      const tags = [undefined];
      Object.defineProperty(tags, 0, { get() {
        calls++; pool.close(); client.close();
        if (fail) throw sentinel;
        return 'sensor';
      } });
      const pending = pool.appendView({ value: 42 }, tags);
      if (fail) await assert.rejects(pending, error => error === sentinel);
      else {
        const view = await pending;
        assert.equal(view.data.value, 42);
        assert.deepEqual([...view.meta.tags], ['sensor']);
      }
      assert.equal(calls, 1);
    });
  }
});


test('array holes use inherited values and serializers receive actual index keys', async t => {
  const { pool } = fixture(t);
  const values = [1, , 3];
  Object.setPrototypeOf(values, Object.create(Array.prototype, { 1: { get() { return 7; } } }));
  const keys = [];
  const serialized = Array.from({ length: 12 }, () => ({ toJSON(key) { keys.push(key); return key; } }));
  const view = await pool.appendView({ values, serialized });
  assert.deepEqual([...view.data.values], [1, 7, 3]);
  assert.deepEqual([...view.data.serialized], Array.from({ length: 12 }, (_, i) => String(i)));
  assert.deepEqual(keys, Array.from({ length: 12 }, (_, i) => String(i)));
});

test('BigInt prototype serializers run once with their primitive receiver', async t => {
  const { pool } = fixture(t);
  const before = Object.getOwnPropertyDescriptor(BigInt.prototype, 'toJSON');
  let calls = 0;
  Object.defineProperty(BigInt.prototype, 'toJSON', { configurable: true, value: function(key) {
    'use strict';
    assert.equal(typeof this, 'bigint'); calls++;
    return key + ':' + this.toString();
  } });
  try {
    const view = await pool.appendView({ integer: 123n });
    assert.equal(view.data.integer, 'integer:123');
    assert.equal(calls, 1);
  } finally {
    if (before) Object.defineProperty(BigInt.prototype, 'toJSON', before);
    else delete BigInt.prototype.toJSON;
  }
});


test('shared-backed byte input requires an ordinary caller-owned copy', async t => {
  const { pool } = fixture(t);
  const bytes = new Uint8Array(new SharedArrayBuffer(4));
  bytes[0] = 7;
  await assert.rejects(pool.appendView({ bytes }), /shared-backed/);
  await assert.rejects(pool.appendView({ bytes: Buffer.from(bytes.buffer) }), /shared-backed/);
  assert.equal((await pool.appendView({ bytes: Uint8Array.from(bytes) })).data.bytes[0], 7);
});


test('BigInt serializer getters observe primitive receivers and raw JSON stays explicit', async t => {
  const { pool } = fixture(t);
  const before = Object.getOwnPropertyDescriptor(BigInt.prototype, 'toJSON');
  let getters = 0;
  Object.defineProperty(BigInt.prototype, 'toJSON', { configurable: true, get: function() {
    'use strict';
    assert.equal(typeof this, 'bigint'); getters++;
    return function(key) { 'use strict'; return key + ':' + this.toString(); };
  } });
  try {
    const view = await pool.appendView({ value: 17n });
    assert.equal(view.data.value, 'value:17');
    assert.equal(getters, 1);
  } finally {
    if (before) Object.defineProperty(BigInt.prototype, 'toJSON', before);
    else delete BigInt.prototype.toJSON;
  }
  await assert.rejects(pool.appendView({ value: JSON.rawJSON('1') }), /explicit JSON/);
});

test('native receipts report backing capacity and JSON-produced copies use the same safe path', async t => {
  const { pool } = fixture(t);
  const view = await pool.appendView({ text: 'x'.repeat(65536) });
  const owner = valueReference(view.data).backend.owner;
  assert.ok(owner.retainedBytes() > owner.byteLength());
  const forwarded = await pool.appendView(view);
  assert.equal(valueReference(forwarded.data).backend.owner.retainedBytes(), owner.retainedBytes());
  const target = pool._inner.prepareAppend(0);
  const jsonOwner = await target.json('{"value":3}', []);
  const jsonView = createNativeMessage(jsonOwner);
  const ref = valueReference(jsonView.data);
  let calls = 0;
  assert.deepEqual(jsonOwner.materialize(ref.node, text => { calls++; return JSON.parse(text); }), { value: 3 });
  assert.equal(calls, 1);
});


test('input exceptions preserve every thrown value without coercing it', async t => {
  const { pool } = fixture(t);
  let coerced = 0;
  const object = { toString() { coerced++; throw new Error('must not stringify an exception'); } };
  for (const thrown of [undefined, null, 17, 'text', Symbol('error'), object]) {
    try {
      await pool.appendView({ get value() { throw thrown; } });
      assert.fail('expected rejection');
    } catch (error) { assert.equal(error, thrown); }
  }
  assert.equal(coerced, 0);
  assert.equal((await pool.appendView({ good: true })).seq, 1n);
});
