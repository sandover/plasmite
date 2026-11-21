const test = require('node:test');
const assert = require('node:assert/strict');
const { createMessageView, toNative, toJson } = require('../views');

// A checked backend substitute exercises the language protocols independently
// of the native ABI. Integration tests supply the real immutable owner.
function owner(data, meta = { tags: ['sensor'] }) {
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
  const root = get({ data, meta });
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

test('a message reads the server sender without reading its data', () => {
  const backend = owner({ temperature: 21 }, { tags: ['sensor'], sender: 'weather station' });
  const message = createMessageView(backend, 4n, 0n);
  assert.equal(message.sender, 'weather station');
  assert.deepEqual(message.tags, ['sensor']);
  assert.equal(backend.nativeCalls(), 0);
  assert.equal(toNative(message).meta.sender, message.sender);
  assert.equal(JSON.parse(toJson(message)).meta.sender, message.sender);
});

test('field looks up one value, shares retained children, and returns undefined for missing keys', () => {
  const backend = owner({ metrics: { power: 42 }, inactive: false, empty: null });
  const message = createMessageView(backend, 7n, 0n);
  const metrics = message.field('metrics');
  assert.equal(metrics.power, 42);
  assert.equal(metrics, message.field('metrics'));
  assert.equal(message.field('inactive'), false);
  assert.equal(message.field('empty'), null);
  assert.equal(message.field('missing'), undefined);
  assert.equal(backend.nativeCalls(), 0);
});

test('data decodes once to an ordinary detached object', () => {
  const backend = owner({ metrics: { power: 42 }, samples: [{ value: 1 }, { value: 2 }] });
  const message = createMessageView(backend, 17n, 1234567890n);
  const data = message.data;
  assert.deepEqual(data, { metrics: { power: 42 }, samples: [{ value: 1 }, { value: 2 }] });
  assert.equal(Object.getPrototypeOf(data), Object.prototype);
  assert.equal(data, message.data);
  data.metrics.power = 1;
  assert.equal(message.field('metrics').power, 42);
  assert.equal(JSON.parse(toJson(message)).data.metrics.power, 42);
  assert.equal(backend.nativeCalls(), 1);
});

test('explicit native conversion builds only the requested subtree', () => {
  const backend = owner({ metrics: { power: 42 } });
  const m = createMessageView(backend, 17n, 1234567890n);
  const metrics = m.field('metrics');
  const native = toNative(metrics);
  native.power = 1;
  assert.equal(metrics.power, 42);
  assert.equal(backend.nativeCalls(), 1);
  assert.equal(toNative(m).time, m.time);
  assert.equal(JSON.parse(toJson(m)).data.metrics.power, 42);
});

test('reflection and dangerous names remain ordinary data', () => {
  const data = JSON.parse('{"__proto__":{"x":1},"constructor":2,"toJSON":3,"then":4,"value":null,"�":5}');
  const value = createMessageView(owner({ nested: data }), 1n, 0n).field('nested');
  assert.equal(Object.getPrototypeOf(value), null);
  assert.equal(Object.hasOwn(value, '__proto__'), true);
  assert.equal(value.__proto__.x, 1);
  assert.equal(value.constructor, 2);
  assert.equal(value.toJSON, 3);
  assert.equal(value.value, null);
  assert.equal(value.missing, undefined);
  assert.equal(value['bad\0key'], undefined);
  assert.equal(value['\ud800'], undefined);
  assert.equal('\ud800' in value, false);
  assert.equal(value['�'], 5);
  assert.equal('bad\0key' in value, false);
  assert.deepEqual(Object.keys(value), Object.keys(data));
  assert.equal(Object.hasOwn(toNative(value), '__proto__'), true);
  assert.equal(JSON.parse(JSON.stringify(value)).then, 4);
});

test('mutation, freeze, seal and cloning fail explicitly on sparse fields', () => {
  const view = createMessageView(owner({ child: { items: [1] } }), 1n, 0n).field('child');
  for (const action of [() => { view.x = 1; }, () => delete view.items,
    () => view.items.push(2), () => Object.defineProperty(view, 'x', { value: 1 }),
    () => Object.setPrototypeOf(view, {}), () => Object.freeze(view),
    () => Object.seal(view.items)]) assert.throws(action, TypeError);
  assert.throws(() => structuredClone(view), { name: 'DataCloneError' });
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
  const backend = owner({ selected: { value: 42, items: [1, 2] } });
  let lookups = 0, indices = 0;
  const lookup = backend.lookup, at = backend.at;
  backend.lookup = (...args) => { lookups++; return lookup(...args); };
  backend.at = (...args) => { indices++; return at(...args); };
  const m = createMessageView(backend, 1n, 0n);
  const data = m.field('selected'), items = data.items;
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

test('fixed root descriptors stay lazy and never repeat immutable lookups', () => {
  const backend = owner({ metrics: { power: 42 } });
  const root = backend.root, lookup = backend.lookup;
  let roots = 0;
  const fields = [];
  backend.root = () => { roots++; return root(); };
  backend.lookup = (node, key) => { fields.push(key); return lookup(node, key); };
  const m = createMessageView(backend, 17n, 123n);
  assert.equal(m.seq, 17n);
  assert.equal(roots, 0);
  const data = m.data, meta = m.meta;
  assert.equal(m.data, data);
  assert.equal(m.meta, meta);
  assert.equal(roots, 1);
  assert.deepEqual(fields, ['data', 'meta']);
});


test('timestamp text uses canonical RFC3339 fractions throughout the u64 range', () => {
  for (const [ns, expected] of [
    [0n, '1970-01-01T00:00:00.000000000Z'],
    [1000000000n, '1970-01-01T00:00:01.000000000Z'],
    [1n, '1970-01-01T00:00:00.000000001Z'],
    [1234567890n, '1970-01-01T00:00:01.234567890Z'],
    [18446744073709551615n, '2554-07-21T23:34:33.709551615Z'],
  ]) {
    const view = createMessageView(owner({}), 1n, ns);
    assert.equal(JSON.parse(toJson(view)).time, expected);
    assert.equal(toNative(view).time, view.time);
    assert.equal(JSON.parse(toJson(view)).time, expected);
  }
});
