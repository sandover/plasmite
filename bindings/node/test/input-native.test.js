const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { Client, ErrorKind, toNative, toJson } = require('..');
const { valueReference, createNativeMessage, createMessageView, messageReference } = require('../views');

function fixture(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-node-input-'));
  const client = new Client(dir), pool = client.createPool('input', 1024 * 1024);
  t.after(() => { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  return { client, pool };
}

test('foreign checked backends import once and preserve exact subtrees', async t => {
  const { pool } = fixture(t);
  const data = { exact: 9223372036854775807n, bytes: Buffer.from([0, 255]), nested: { value: 17 } };
  const source = await pool.feed(data, { tags: ['source'] });
  const original = messageReference(source).backend;
  let copies = 0;
  const backend = { ...original, encodedBytes() { copies++; return original.owner.encodedBytes(); } };
  delete backend.owner;
  const foreign = createMessageView(backend, source.seq, source.time);
  const forwarded = await pool.feed(foreign);
  assert.deepEqual(toNative(forwarded.data), data);
  assert.deepEqual(toNative(forwarded.meta.tags), ['source']);
  const mixed = await pool.feed({ subset: foreign.data, nested: foreign.data.nested });
  assert.deepEqual(toNative(mixed.data), { subset: data, nested: { value: 17 } });
  assert.equal(copies, 1);
  const exported = source.raw;
  exported.fill(0);
  assert.equal(forwarded.data.exact, data.exact);
  assert.equal(foreign.data.nested.value, 17);
});

test('native input keeps exact numeric values, bytes and a detached receipt', async t => {
  const { pool } = fixture(t);
  const input = { integer: 42, smallBig: 42n, maximum: 9223372036854775807n,
    minimum: -9223372036854775808n, binary64: 9007199254740992,
    zero: -0, text: '雪😀�\0x', buffer: Buffer.from([0, 255]), bytes: new Uint8Array([1, 2]) };
  const pending = pool.feed(input, { tags: [' z ', 'a', 'a', '\0'] });
  input.integer = 0; input.buffer[0] = 9; input.bytes[0] = 9;
  const view = await pending;
  assert.equal(view.seq, 1n);
  assert.equal(typeof view.time, 'bigint');
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
  const view = await pool.feed(input(actualLog));
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
  const view = await pool.feed(input);
  assert.deepEqual(toNative(view.data), { number: 3, string: 'text', boolean: true, bigint: 7,
    buffer: { custom: 'buffer' }, subclass: { subclass: 'subclass' }, date: new Date(0).toJSON() });
  const invalid = new Number(1);
  invalid[Symbol.toPrimitive] = () => 1n;
  await assert.rejects(pool.feed({ invalid }), TypeError);
});

test('native input preserves thrown values and rejects before publication', async t => {
  const { pool } = fixture(t);
  const sentinel = Object.assign(new Error('from getter'), { kind: 'Usage' });
  await assert.rejects(pool.feed({ get value() { throw sentinel; } }), error => error === sentinel);
  const cycle = {}; cycle.self = cycle;
  for (const value of [cycle, { value: 9223372036854775808n }, { value: Infinity },
    { value: '\ud800' }, { '\udfff': 1 }]) {
    await assert.rejects(pool.feed(value), TypeError);
  }
  for (const value of [[], 1, Buffer.from([1])]) {
    await assert.rejects(pool.feed(value), { kind: ErrorKind.Usage });
  }
  assert.equal((await pool.feed({ good: '�' })).seq, 1n);
});

test('native input invokes getters once across writer growth', async t => {
  const { pool } = fixture(t);
  let calls = 0;
  const input = { get value() { calls++; return 'x'.repeat(65536); } };
  const view = await pool.feed(input);
  assert.equal(calls, 1);
  assert.equal(view.data.value.length, 65536);
  const root = function () {};
  root.toJSON = key => ({ key });
  assert.equal((await pool.feed(root)).data.key, '');
});

test('native input retains its private target when hooks close the pool', async t => {
  for (const mode of ['getter', 'toJSON']) {
    await t.test(mode, async t => {
      const { pool, client } = fixture(t);
      const input = mode === 'getter'
        ? { get value() { pool.close(); client.close(); return 42; } }
        : { toJSON() { pool.close(); client.close(); return { value: 42 }; } };
      const view = await pool.feed(input);
      assert.equal(view.data.value, 42);
      await assert.rejects(pool._getView(view.seq), error => error.kind === ErrorKind.Usage);
    });
  }
});

test('queued native appends finish after close and retain their committed snapshots', async t => {
  const { pool, client } = fixture(t);
  const pending = [1, 2, 3].map(value => pool.feed({ value }));
  pool.close(); client.close();
  const views = await Promise.all(pending);
  assert.deepEqual(views.map(view => view.data.value), [1, 2, 3]);
  assert.equal(new Set(views.map(view => view.seq)).size, 3);
});

test('native input copies checked subtrees without losing native bytes', async t => {
  const { pool } = fixture(t);
  const first = await pool.feed({ nested: { value: 3n }, bytes: Buffer.from([0, 255]) });
  const copied = await pool.feed({ subtree: first.data.nested, bytes: first.data.bytes });
  assert.equal(copied.data.subtree.value, 3);
  assert.deepEqual([...copied.data.bytes], [0, 255]);
  const root = await pool.feed(first.data, { tags: ['retagged'] });
  assert.equal(root.data.nested.value, 3);
  assert.deepEqual([...root.data.bytes], [0, 255]);
  assert.deepEqual([...root.meta.tags], ['retagged']);
});

test('whole-message forwarding preserves payload and optional retagging copies data', async t => {
  const { pool } = fixture(t);
  const first = await pool.feed({ value: 9223372036854775807n, bytes: Buffer.from([7, 8]) }, { tags: ['original'] });
  const forwarded = await pool.feed(first);
  const retagged = await pool.feed(first.data, { tags: ['changed'] });
  assert.equal(forwarded.seq, 2n);
  assert.equal(retagged.seq, 3n);
  assert.equal(forwarded.data.value, 9223372036854775807n);
  assert.deepEqual([...forwarded.data.bytes], [7, 8]);
  assert.deepEqual([...forwarded.meta.tags], ['original']);
  assert.deepEqual([...retagged.meta.tags], ['changed']);
  assert.deepEqual([...retagged.data.bytes], [7, 8]);
  const ordinary = await pool.feed({ seq: 99, meta: { tags: ['data'] }, data: { value: 1 } });
  assert.equal(ordinary.data.seq, 99);
  assert.equal(ordinary.data.data.value, 1);
});

test('forwarded children survive GC of original and intermediate wrappers', { skip: !global.gc }, async t => {
  const { pool, client } = fixture(t);
  let original = await pool.feed({ bytes: Buffer.alloc(65536, 7) });
  let intermediate = await pool.feed(original);
  let last = await pool.feed(intermediate);
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
  await assert.rejects(pool.feed({ get value() {
    pool.close(); client.close(); throw sentinel;
  } }), error => error === sentinel);
  await assert.rejects(pool.feed({}), error => error.kind === ErrorKind.Usage);
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
      const pending = pool.feed({ value: 42 }, { tags: tags });
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
  const view = await pool.feed({ values, serialized });
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
    const view = await pool.feed({ integer: 123n });
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
  await assert.rejects(pool.feed({ bytes }), /shared-backed/);
  await assert.rejects(pool.feed({ bytes: Buffer.from(bytes.buffer) }), /shared-backed/);
  assert.equal((await pool.feed({ bytes: Uint8Array.from(bytes) })).data.bytes[0], 7);
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
    const view = await pool.feed({ value: 17n });
    assert.equal(view.data.value, 'value:17');
    assert.equal(getters, 1);
  } finally {
    if (before) Object.defineProperty(BigInt.prototype, 'toJSON', before);
    else delete BigInt.prototype.toJSON;
  }
  await assert.rejects(pool.feed({ value: JSON.rawJSON('1') }), /explicit JSON/);
});

test('native receipts report backing capacity and JSON-produced copies use the same safe path', async t => {
  const { pool } = fixture(t);
  const view = await pool.feed({ text: 'x'.repeat(65536) });
  const owner = messageReference(view).backend.owner;
  assert.ok(owner.retainedBytes() > owner.byteLength());
  const forwarded = await pool.feed(view);
  assert.equal(messageReference(forwarded).backend.owner.retainedBytes(), owner.retainedBytes());
  const target = pool._inner.prepareAppend(0);
  const jsonOwner = await target.json('{"value":3}', []);
  const jsonView = createNativeMessage(jsonOwner);
  const node = messageReference(jsonView).data().node;
  let calls = 0;
  assert.deepEqual(jsonOwner.materialize(node, text => { calls++; return JSON.parse(text); }), { value: 3 });
  assert.equal(calls, 1);
});


test('input exceptions preserve every thrown value without coercing it', async t => {
  const { pool } = fixture(t);
  let coerced = 0;
  const object = { toString() { coerced++; throw new Error('must not stringify an exception'); } };
  for (const thrown of [undefined, null, 17, 'text', Symbol('error'), object]) {
    try {
      await pool.feed({ get value() { throw thrown; } });
      assert.fail('expected rejection');
    } catch (error) { assert.equal(error, thrown); }
  }
  assert.equal(coerced, 0);
  assert.equal((await pool.feed({ good: true })).seq, 1n);
});

test('captured primitives and bytes keep their value at the visit', async t => {
  const { pool } = fixture(t);
  const earlier = { value: 1 }, bytes = Buffer.from([1, 2]);
  const view = await pool.feed({ earlier, bytes, get later() { earlier.value = 9; bytes[0] = 9; return true; } });
  assert.equal(view.data.earlier.value, 1);
  assert.deepEqual([...toNative(view.data.bytes)], [1, 2]);
});

test('capture batches preserve traversal and reentrant appends', async t => {
  const { pool } = fixture(t);
  let reads = 0, nested;
  const rows = Array.from({ length: 700 }, (_, number) => ({ get value() {
    reads++;
    if (number === 170) nested = pool.feed({ inner: true });
    return number;
  } }));
  const result = await pool.feed({ rows });
  assert.equal(reads, 700);
  assert.equal(result.data.rows.length, 700);
  assert.equal(result.data.rows[0].value, 0);
  assert.equal(result.data.rows[170].value, 170);
  assert.equal(result.data.rows[699].value, 699);
  assert.equal((await nested).data.inner, true);
});

test('key failures preserve Get and hook order before later fields', async t => {
  const { pool } = fixture(t);
  const unicode = [], nul = [];
  const invalidKey = Object.defineProperty({}, '\ud800', { enumerable: true, get() {
    unicode.push('get'); return { toJSON() { unicode.push('hook'); return 1; } };
  } });
  Object.defineProperty(invalidKey, 'later', { enumerable: true, get() { unicode.push('later'); return 2; } });
  await assert.rejects(pool.feed(invalidKey), /surrogate/);
  assert.deepEqual(unicode, ['get']);
  await assert.rejects(pool.feed({ ['\0']: { toJSON() { nul.push('hook'); return 1; } }, get later() { nul.push('later'); return 2; } }), /NUL/);
  assert.deepEqual(nul, ['hook']);
  const ignored = await pool.feed({ ['\0']: undefined, value: 3 });
  assert.equal(ignored.data.value, 3);
});

test('checked subtree depth errors precede later getters', async t => {
  const { pool } = fixture(t);
  let deep = { leaf: 1 };
  for (let i = 0; i < 30; i++) deep = { nested: deep };
  const source = await pool.feed(deep);
  let later = 0;
  await assert.rejects(pool.feed({ copy: source.data, get later() { later++; return 2; } }), /nest|depth/i);
  assert.equal(later, 0);
  assert.equal((await pool.feed({ good: true })).seq, source.seq + 1n);
});

test('mixed scalar commands remain exact across batches and buffer reuse', async t => {
  const { pool } = fixture(t);
  const values = Array.from({ length: 1200 }, (_, i) => [null, false, true, -0, 0, 1, 3.5, 9007199254740991, 9007199254740992, 17n, 9223372036854775807n, -9223372036854775808n][i % 12]);
  const expected = values.map(value => value === 17n ? 17 : value);
  for (let i = 0; i < 2; i++) {
    const view = await pool.feed({ values });
    assert.deepEqual(toNative(view.data.values), expected);
  }
});

test('real portable owners import once for native whole, root, mixed and retagged input', async t => {
  const { pool } = fixture(t);
  const portable = require('../portable');
  const { configureNativeInput } = require('../input');
  const addon = require('../native/' + process.platform + '-' + process.arch + '/index.node');
  const data = { exact: 9223372036854775807n, bytes: Buffer.from([0, 255]), nested: { value: 17 } };
  const bytes = await portable.encodeMessage(data, ['source']);
  const source = await portable.decodeMessage(bytes, 77n, 0n);
  let imports = 0;
  configureNativeInput(encoded => { imports++; return addon.nativeViewFromBytes(encoded); }, addon.NativeView);
  try {
    const whole = await pool.feed(source);
    const root = await pool.feed(source.data);
    const retagged = await pool.feed(source.data, { tags: ['retagged'] });
    const mixed = await pool.feed({ subset: source.data.nested, payload: source.data });
    assert.deepEqual(toNative(whole.data), data);
    assert.deepEqual(toNative(whole.meta.tags), ['source']);
    assert.deepEqual(toNative(root.data), data);
    assert.deepEqual(toNative(root.meta.tags), []);
    assert.deepEqual(toNative(retagged.data), data);
    assert.deepEqual(toNative(retagged.meta.tags), ['retagged']);
    assert.deepEqual(toNative(mixed.data), { subset: data.nested, payload: data });
    assert.equal(imports, 1);
    await pool.feed(whole);
    assert.equal(imports, 1, 'native owners bypass import');
  } finally { configureNativeInput(addon.nativeViewFromBytes, addon.NativeView); }
});
