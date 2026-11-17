const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const portable = require('../portable');
const { toNative, toJson, valueReference, messageReference } = require('../views');
const round = async (data, tags) => portable.decodeMessage(await portable.encodeMessage(data, tags), 7n, 1234567890n);

test('remote-only import stays lazy and the checked artifact needs no runtime imports', () => {
  const module = new WebAssembly.Module(fs.readFileSync(path.join(__dirname, '../portable/plasmite_core.wasm')));
  assert.deepEqual(WebAssembly.Module.imports(module), []);
  execFileSync(process.execPath, ['-e', `const p=require(${JSON.stringify(require.resolve('../portable'))});
    if(p.diagnostics().initialized)throw new Error('eager initialization');
    const {RemoteClient}=require(${JSON.stringify(require.resolve('..'))});
    new RemoteClient('http://127.0.0.1:1');`]);
});

test('portable proxies keep natural fields, arrays, stable children and explicit copies', async () => {
  const message = await round({ metrics: { power: 42 }, samples: [{ value: 1 }, { value: 2 }], nil: null });
  assert.equal(message.seq, 7n);
  assert.equal(message.time, 1234567890n);
  assert.equal(JSON.parse(toJson(message)).time, '1970-01-01T00:00:01.234567890Z');
  assert.equal(message.data.metrics.power, 42);
  assert.equal(message.data.metrics, message.data.metrics);
  assert.equal({ ...message.data }.metrics, message.data.metrics);
  assert.equal(Array.isArray(message.data.samples), true);
  assert.deepEqual(message.data.samples.map(x => x.value), [1, 2]);
  assert.equal(message.data.samples.at(-1).value, 2);
  assert.equal(message.data.samples.slice(0, 1)[0], message.data.samples[0]);
  assert.deepEqual(Object.keys(message.data.samples), ['0', '1']);
  assert.equal(Object.hasOwn(message.data, 'nil'), true);
  assert.equal(message.data.absent, undefined);
  assert.equal('bad\0key' in message.data, false);
  assert.equal(message.data['\ud800'], undefined);
  const copy = toNative(message.data); copy.metrics.power = 1;
  assert.equal(message.data.metrics.power, 42);
  assert.deepEqual(JSON.parse(toJson(message)).meta, { tags: [] });
  for (const action of [() => { message.data.nil = 1; }, () => delete message.data.nil,
    () => message.data.samples.push(4), () => Object.freeze(message.data),
    () => Object.seal(message.data.samples), () => Object.setPrototypeOf(message.data, {})]) assert.throws(action, TypeError);
  assert.throws(() => structuredClone(message.data), { name: 'DataCloneError' });
});

test('portable numbers, strings, binary and dangerous property names match native views', async () => {
  const input = JSON.parse('{"__proto__":{"x":1},"constructor":2,"toJSON":3,"then":null}');
  Object.assign(input, { minimum: -(1n << 63n), maximum: (1n << 63n) - 1n, safe: 9007199254740991,
    negativeSafe: -9007199254740991, wide: 9007199254740992n, wideFloat: 9007199254740992,
    smallBig: 7n, zero: -0, text: '雪😀�\0x', bytes: Buffer.from([0, 255]) });
  const message = await round(input, [' z ', 'a', 'a', '\0']);
  assert.equal(message.data.minimum, -(1n << 63n));
  assert.equal(message.data.maximum, (1n << 63n) - 1n);
  assert.equal(message.data.safe, 9007199254740991);
  assert.equal(message.data.negativeSafe, -9007199254740991);
  assert.equal(message.data.wide, 9007199254740992n);
  assert.equal(typeof message.data.wideFloat, 'number');
  assert.equal(message.data.smallBig, 7);
  assert.equal(Object.is(message.data.zero, -0), true);
  assert.equal(message.data.text, input.text);
  assert.deepEqual([...message.meta.tags], [' z ', 'a', 'a', '\0']);
  assert.deepEqual([...message.data.bytes], [0, 255]);
  assert.equal(toJson(message.data.bytes), '"AP8="');
  const copy = toNative(message.data);
  assert.equal(Object.getPrototypeOf(copy), Object.prototype);
  assert.equal(Object.hasOwn(copy, '__proto__'), true);
  assert.equal(copy.__proto__.x, 1);
  assert.equal(copy.maximum, input.maximum);
  copy.bytes[0] = 99;
  assert.equal(message.data.bytes[0], 0);
});

test('input capture runs before yielding and byte exports never alias the record', async () => {
  const data = { value: 7, bytes: Buffer.from([1, 2]), nested: [3] };
  const pending = portable.encodeMessage(data);
  data.value = 9; data.bytes[0] = 99; data.nested[0] = 99;
  const bytes = await pending;
  const acquiring = portable.decodeMessage(bytes, 1n, 0n);
  bytes.fill(0);
  const message = await acquiring;
  assert.deepEqual(toNative(message.data), { value: 7, bytes: Buffer.from([1, 2]), nested: [3] });
  const exported = messageReference(message).backend.encodedBytes();
  exported.fill(0);
  assert.equal(message.data.value, 7);
});

test('JSON hooks, Proxy traps, holes and boxed input preserve access order', async () => {
  function input(log) {
    const array = new Proxy([1, , undefined, () => {}, Symbol('x')], {
      get(target, key, receiver) { log.push('array.get:' + String(key)); return Reflect.get(target, key, receiver); },
    });
    return new Proxy({ get x() { log.push('getter:x'); return 2; },
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
  const message = await round(input(actualLog));
  assert.deepEqual(toNative(message.data), expected);
  assert.deepEqual(actualLog, expectedLog);
  const buffer = Buffer.from([1, 2]); buffer.toJSON = key => ({ custom: key });
  class Bytes extends Uint8Array { toJSON(key) { return { subclass: key }; } }
  const boolean = new Boolean(true); boolean.valueOf = () => { throw new Error('use internal boolean'); };
  assert.deepEqual(toNative((await round({ number: new Number(3), string: new String('text'), boolean,
    bigint: Object(7n), buffer, subclass: new Bytes([3]), date: new Date(0) })).data),
    { number: 3, string: 'text', boolean: true, bigint: 7, buffer: { custom: 'buffer' },
      subclass: { subclass: 'subclass' }, date: new Date(0).toJSON() });
});

test('malformed keys reject after Get but before child hooks, while empty frames stay Corrupt', async () => {
  const log = [];
  const input = Object.defineProperty({}, '\ud800', { enumerable: true, get() {
    log.push('get'); return { toJSON() { log.push('hook'); return 1; } };
  }});
  await assert.rejects(portable.encodeMessage(input), TypeError);
  assert.deepEqual(log, ['get']);
  const sentinel = new Error('property get wins');
  const throwing = Object.defineProperty({}, '\ud800', { enumerable: true, get() { throw sentinel; }});
  await assert.rejects(portable.encodeMessage(throwing), error => error === sentinel);
  await assert.rejects(portable.decodeMessage(Buffer.alloc(0), 1n, 0n), error => error.kind === 7);
});

test('callbacks run once through growth, preserve thrown identity and allow reentrant encoding', async () => {
  let count = 0;
  const inner = [];
  const message = await round({ get value() { count++; inner.push(portable.encodeMessage({ inner: 1 })); return 'x'.repeat(65536); } });
  assert.equal(count, 1);
  assert.equal(message.data.value.length, 65536);
  assert.equal(toNative((await portable.decodeMessage(await inner[0], 1n, 0n)).data).inner, 1);
  const sentinel = Object.assign(new Error('from getter'), { kind: 'Usage' });
  await assert.rejects(portable.encodeMessage({ get value() { throw sentinel; } }), error => error === sentinel);
  const thrown = Symbol('sentinel');
  await portable.encodeMessage({ get value() { throw thrown; } }).then(() => assert.fail('accepted'), error => assert.equal(error, thrown));
});

test('checked input rejects cycles, invalid roots/numbers/text/depth without retained owners', async () => {
  const cycle = {}; cycle.self = cycle;
  const before = portable.diagnostics().ownerCount;
  for (const value of [cycle, [], 1, Buffer.from([1]), { x: 1n << 63n }, { x: -(1n << 63n) - 1n },
    { x: Infinity }, { x: NaN }, { x: '\ud800' }, { '\udfff': 1 }, { 'a\0b': 1 },
    { x: JSON.rawJSON('1') }, { x: new Uint8Array(new SharedArrayBuffer(4)) }]) {
    await assert.rejects(portable.encodeMessage(value));
  }
  assert.ok(portable.diagnostics().ownerCount <= before);
  let data = null;
  for (let i = 0; i < 30; i++) data = [data];
  assert.equal(Array.isArray(toNative((await round({ value: data })).data.value)), true);
  await assert.rejects(portable.encodeMessage({ value: [data] }), /nest|depth/);
  assert.ok(portable.diagnostics().ownerCount <= before + 1);
});

test('full/root/subtree forwarding keeps exact data, tags and node offsets', async () => {
  const original = await round({ nested: { integer: (1n << 63n) - 1n }, array: [{ value: 1 }, { value: 2 }], binary: Buffer.from([3, 4]) }, ['source', '\0']);
  const whole = await portable.encodeMessage(original);
  assert.deepEqual(whole, messageReference(original).backend.encodedBytes());
  const forwarded = await portable.decodeMessage(whole, 8n, 0n);
  assert.deepEqual([...forwarded.meta.tags], ['source', '\0']);
  const retagged = await round(original, []);
  assert.deepEqual([...retagged.meta.tags], []);
  assert.deepEqual(toNative(retagged.data), toNative(original.data));
  const root = await round(original.data.nested);
  assert.equal(root.data.integer, (1n << 63n) - 1n);
  const mixed = await round({ nested: original.data.nested, array: original.data.array, binary: original.data.binary });
  assert.deepEqual(toNative(mixed.data), toNative(original.data));
  const reidentified = portable.messageWithIdentity(original, 99n, 999n);
  assert.equal(reidentified.seq, 99n);
  assert.equal(messageReference(reidentified).backend, messageReference(original).backend);
  assert.equal(valueReference(reidentified.data).node, valueReference(original.data).node);
});

test('host-generated bytes, collision duplicates and malformed indexes use the shared checker', async () => {
  const host = fs.readFileSync(path.join(__dirname, '../../python/tests/fixtures/extra-meta.lite3'));
  const message = await portable.decodeMessage(host, 9n, 10n);
  assert.equal(message.data.value, 1);
  assert.equal(message.meta.extra, 7);
  assert.deepEqual(toNative(message).meta, { tags: [] });
  const encoded = await portable.encodeMessage({ ab: 1, bA: 2 });
  const original = await portable.decodeMessage(encoded, 1n, 0n);
  const keys = Object.keys(original.data);
  const changed = Buffer.from(encoded);
  const offset = changed.indexOf(Buffer.from(keys[1] + '\0'));
  assert.ok(offset > 0);
  changed.write(keys[0], offset, 'utf8');
  const duplicate = await portable.decodeMessage(changed, 1n, 0n);
  assert.deepEqual(Object.keys(duplicate.data), [keys[0]]);
  assert.equal(duplicate.data[keys[0]], original.data[keys[1]]);
  assert.deepEqual(toNative(duplicate.data), JSON.parse(toJson(duplicate.data)));
  const forged = Buffer.from(encoded); forged.write('zz', forged.indexOf(Buffer.from('ab\0')), 'utf8');
  for (const bytes of [forged, encoded.subarray(0, encoded.length - 1), Buffer.alloc(96), Buffer.from([1])]) {
    await assert.rejects(portable.decodeMessage(bytes, 1n, 0n), error => error.kind === 7);
  }
});

test('memory growth preserves live children, independent snapshots and pending iterators', async () => {
  let message = await round({ nested: { value: 7 }, array: Array.from({ length: 40 }, (_, i) => i) });
  const child = message.data.nested;
  const iterator = message.data.array[Symbol.iterator]();
  assert.equal(iterator.next().value, 0);
  message = null;
  const before = portable.diagnostics().linearBytes;
  const payload = await portable.encodeMessage({ text: 'x'.repeat(1024 * 1024) });
  assert.ok(portable.diagnostics().linearBytes > before);
  payload.fill(0);
  assert.equal(child.value, 7);
  assert.deepEqual([...iterator], Array.from({ length: 39 }, (_, i) => i + 1));
  const other = await round({ nested: { value: 9 } });
  assert.equal(other.data.nested.value, 9);
  assert.equal(child.value, 7);
});

test('append preparation retains one encoding, snapshots once and cleans failed requests', async () => {
  let calls = 0;
  const bytes = Buffer.from([1, 2]);
  const input = { get value() { calls++; return { toJSON() { calls++; return 42; } }; }, bytes };
  const before = portable.diagnostics().ownerCount;
  const prepared = portable.prepareAppend(input, ['ready']);
  assert.equal(calls, 2);
  assert.equal(portable.diagnostics().ownerCount, before + 1);
  bytes.fill(9);
  assert.deepEqual([...prepared.message.data.bytes], [1, 2]);
  const committed = portable.messageWithIdentity(prepared.message, 19n, 123n);
  assert.equal(committed.seq, 19n);
  assert.equal(committed.data.value, 42);
  assert.equal(messageReference(committed).backend, messageReference(prepared.message).backend);
  assert.equal(portable.diagnostics().ownerCount, before + 1);
  // The HTTP body owns its bytes and cannot mutate the retained message.
  prepared.payload.fill(0);
  assert.equal(committed.data.value, 42);
  const failed = portable.prepareAppend({ value: 5 });
  assert.equal(portable.diagnostics().ownerCount, before + 2);
  try { throw new Error('request failed'); } catch { failed.discard(); }
  failed.discard();
  assert.equal(portable.diagnostics().ownerCount, before + 1);
  const failedRetag = portable.prepareAppend(committed, []);
  failedRetag.discard();
  assert.equal(committed.data.value, 42);
  assert.equal(calls, 2);
  assert.equal(portable.diagnostics().ownerCount, before + 1);
  // Test cleanup only: this prepared message has no external consumer.
  prepared.discard();
  assert.equal(portable.diagnostics().ownerCount, before);
});

test('append preparation forwards caller owners and retags through one new owner', async () => {
  const source = await round({ nested: { value: 3 }, exact: (1n << 63n) - 1n }, ['source']);
  const sources = [source];
  if (process.env.PLASMITE_PORTABLE_TEST_ADDON) {
    const addon = require(process.env.PLASMITE_PORTABLE_TEST_ADDON);
    const { createNativeMessage } = require('../views');
    sources.push(createNativeMessage(addon.nativeViewFromBytes(await portable.encodeMessage(source))));
  }
  for (const original of sources) {
    const before = portable.diagnostics().ownerCount;
    const whole = portable.prepareAppend(original);
    assert.equal(whole.message, original);
    assert.equal(messageReference(whole.message).backend, messageReference(original).backend);
    whole.discard(); whole.discard();
    assert.equal(original.data.nested.value, 3);
    assert.equal(portable.diagnostics().ownerCount, before);
    const retagged = portable.prepareAppend(original, []);
    assert.deepEqual([...retagged.message.meta.tags], []);
    assert.deepEqual(toNative(retagged.message.data), toNative(original.data));
    assert.equal(portable.diagnostics().ownerCount, before + 1);
    retagged.discard();
    assert.equal(portable.diagnostics().ownerCount, before);
    assert.deepEqual([...original.meta.tags], ['source']);
  }
});
