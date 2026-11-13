const test = require('node:test');
const assert = require('node:assert/strict');
const { createMessageView, toNative, toJson } = require('../views');

// A checked backend substitute exercises the language protocols independently
// of the native ABI. Integration tests supply the real immutable owner.
function owner(data) {
  const values = new Map();
  let next = 0;
  function describe(value) {
    if (value === null || typeof value !== 'object') return value;
    const node = next++;
    values.set(node, value);
    return { kind: Array.isArray(value) ? 'array' : 'object', node,
      count: Object.keys(value).length };
  }
  const descriptors = new WeakMap();
  function get(value) {
    if (value === null || typeof value !== 'object') return value;
    if (!descriptors.has(value)) descriptors.set(value, describe(value));
    return descriptors.get(value);
  }
  const root = get({ data, meta: { tags: ['sensor'] } });
  let nativeCalls = 0;
  return {
    root: () => root,
    lookup: (node, key) => Object.hasOwn(values.get(node), key) ? get(values.get(node)[key]) : undefined,
    at: (node, index) => get(values.get(node)[index]),
    keys: node => Object.keys(values.get(node)),
    has: (node, key) => Object.hasOwn(values.get(node), key),
    *iterate(node) { for (const item of values.get(node)) yield get(item); },
    materialize(node) { nativeCalls++; return structuredClone(values.get(node)); },
    json: node => JSON.stringify(values.get(node)),
    nativeCalls: () => nativeCalls,
  };
}

test('natural object and array access preserves child identity and shallow copies', () => {
  const backend = owner({ metrics: { power: 42 }, samples: [{ value: 1 }, { value: 2 }] });
  const m = createMessageView(backend, 17n, 1234567890n);
  assert.equal(m.data.metrics.power, 42);
  assert.equal(m.data.metrics, m.data.metrics);
  assert.equal({ ...m.data }.metrics, m.data.metrics);
  assert.equal(Array.isArray(m.data.samples), true);
  assert.deepEqual([...m.data.samples].map(x => x.value), [1, 2]);
  assert.deepEqual(m.data.samples.map(x => x.value), [1, 2]);
  assert.equal(m.data.samples.at(-1).value, 2);
  assert.equal(m.data.samples.slice(0, 1)[0], m.data.samples[0]);
  assert.deepEqual(Object.keys(m.data.samples), ['0', '1']);
  assert.deepEqual(Object.getOwnPropertyDescriptor(m.data.samples, 'length'),
    { value: 2, writable: false, enumerable: false, configurable: false });
  assert.equal(backend.nativeCalls(), 0);
});

test('explicit native conversion builds only the requested subtree', () => {
  const backend = owner({ metrics: { power: 42 } });
  const m = createMessageView(backend, 17n, 1234567890n);
  const native = toNative(m.data.metrics);
  native.power = 1;
  assert.equal(m.data.metrics.power, 42);
  assert.equal(backend.nativeCalls(), 1);
  assert.equal(toNative(m).time, '1970-01-01T00:00:01.234567890Z');
  assert.equal(JSON.parse(toJson(m)).data.metrics.power, 42);
});

test('reflection and dangerous names remain ordinary data', () => {
  const data = JSON.parse('{"__proto__":{"x":1},"constructor":2,"toJSON":3,"then":4,"value":null}');
  const m = createMessageView(owner(data), 1n, 0n);
  assert.equal(Object.getPrototypeOf(m.data), null);
  assert.equal(Object.hasOwn(m.data, '__proto__'), true);
  assert.equal(m.data.__proto__.x, 1);
  assert.equal(m.data.constructor, 2);
  assert.equal(m.data.toJSON, 3);
  assert.equal(m.data.value, null);
  assert.equal(m.data.missing, undefined);
  assert.equal(m.data['bad\0key'], undefined);
  assert.equal('bad\0key' in m.data, false);
  assert.deepEqual(Object.keys(m.data), Object.keys(data));
  assert.equal(Object.hasOwn(toNative(m.data), '__proto__'), true);
  assert.equal(JSON.parse(JSON.stringify(m.data)).then, 4);
});

test('mutation, freeze, seal and cloning fail explicitly', () => {
  const m = createMessageView(owner({ items: [1] }), 1n, 0n);
  for (const action of [() => { m.data.x = 1; }, () => delete m.data.items,
    () => m.data.items.push(2), () => Object.defineProperty(m.data, 'x', { value: 1 }),
    () => Object.setPrototypeOf(m.data, {}), () => Object.freeze(m.data),
    () => Object.seal(m.data.items)]) assert.throws(action, TypeError);
  assert.throws(() => structuredClone(m.data), { name: 'DataCloneError' });
  assert.equal(toJson(-0), '-0.0');
  assert.equal(toJson(9223372036854775807n), '9223372036854775807');
});

test('primitive null, false, zero and empty string remain present', () => {
  const m = createMessageView(owner({ mixed: [null, false, 0, ''], value: null }), 1n, 0n);
  assert.deepEqual([...m.data.mixed], [null, false, 0, '']);
  assert.equal(Object.hasOwn(m.data, 'value'), true);
  assert.equal(m.data.value, null);
});

test('presence and dense-array map avoid decoding values twice', () => {
  const backend = owner({ value: 42, items: [1, 2] });
  let lookups = 0, indices = 0;
  const lookup = backend.lookup, at = backend.at;
  backend.lookup = (...args) => { lookups++; return lookup(...args); };
  backend.at = (...args) => { indices++; return at(...args); };
  const m = createMessageView(backend, 1n, 0n);
  const data = m.data, items = data.items;
  lookups = 0;
  assert.equal('value' in data, true);
  assert.equal('missing' in data, false);
  assert.equal(0 in items, true);
  assert.equal(2 in items, false);
  assert.equal('map' in items, true);
  assert.equal(lookups, 0);
  assert.equal(indices, 0);
  assert.deepEqual(items.map(x => x * 2), [2, 4]);
  assert.equal(indices, 2);
});
