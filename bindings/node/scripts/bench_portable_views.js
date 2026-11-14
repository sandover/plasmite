#!/usr/bin/env node
// Focused portable/native/JSON controls. Run timed modes in the central quiet pass.
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const zlib = require('node:zlib');
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const packageRoot = path.resolve(__dirname, '..');
const options = {};
for (let i = 2; i < process.argv.length; i++) {
  const key = process.argv[i].replace(/^--/, '');
  options[key] = ['verify-only', 'worker-cold'].includes(key) ? true : process.argv[++i];
}
if (options.repeats && (!Number.isInteger(Number(options.repeats)) || Number(options.repeats) < 1)) throw new Error('repeats must be a positive integer');
if (options.scale && (!Number.isFinite(Number(options.scale)) || Number(options.scale) <= 0)) throw new Error('scale must be positive');
if (options.mode && !['operations', 'startup', 'retention'].includes(options.mode)) throw new Error('unknown benchmark mode');
const verifyOnly = !!options['verify-only'];
const now = () => verifyOnly ? 0n : process.hrtime.bigint();
const elapsed = start => verifyOnly ? null : Number(now() - start);
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const output = value => {
  const text = JSON.stringify(value, null, 2) + '\n';
  if (options.output) fs.writeFileSync(options.output, text);
  else process.stdout.write(text);
};
function loadNative(loadedAddon) {
  if (!options['native-addon']) return { supported: false, reason: 'no native addon supplied' };
  if (!options['library-file']) throw new Error('--native-addon requires the exact --library-file fingerprint');
  const addonPath = path.resolve(options['native-addon']);
  const libraryPath = path.resolve(options['library-file']);
  process.env.PLASMITE_LIB_DIR = path.dirname(libraryPath);
  const addon = loadedAddon || require(addonPath);
  if (typeof addon.nativeViewFromBytes !== 'function') return { supported: false, reason: 'native addon lacks checked nativeViewFromBytes' };
  const librarySha256 = sha(fs.readFileSync(libraryPath));
  const loaded = process.report.getReport().sharedObjects.filter(name => /^(?:lib)?plasmite\.(?:so|dylib|dll)$/.test(path.basename(name)));
  const libraries = loaded.map(name => ({ path: name, sha256: sha(fs.readFileSync(name)) }));
  if (!libraries.some(item => item.sha256 === librarySha256)) throw new Error('native control did not load the declared shared library');
  const { createNativeMessage } = require('../views');
  return { supported: true, addon, metadata: { addonPath, addonSha256: sha(fs.readFileSync(addonPath)),
    libraryPath, librarySha256, addonBytes: fs.statSync(addonPath).size, libraryBytes: fs.statSync(libraryPath).size, loadedLibraries: libraries },
    decode: bytes => createNativeMessage(addon.nativeViewFromBytes(bytes)) };
}
async function cold() {
  const bytes = fs.readFileSync(options.fixture);
  const json = fs.readFileSync(options.json, 'utf8');
  let started = now();
  const portable = require('../portable');
  const requireNs = elapsed(started);
  started = now();
  const view = await portable.decodeMessage(bytes, 1n, 0n);
  const firstDecodeNs = elapsed(started);
  started = now();
  const sparse = view.data.metrics.power;
  const firstSparseNs = elapsed(started);
  assert.equal(sparse, 42);
  started = now();
  const plain = JSON.parse(json).data.metrics.power;
  const jsonParseSparseNs = elapsed(started);
  assert.equal(plain, sparse);
  let native = { supported: false, reason: 'no native addon supplied' };
  if (options['native-addon']) {
    if (!options['library-file']) throw new Error('native startup control needs --library-file');
    process.env.PLASMITE_LIB_DIR = path.dirname(path.resolve(options['library-file']));
    started = now();
    const addon = require(path.resolve(options['native-addon']));
    const requireNs = elapsed(started);
    const control = loadNative(addon);
    if (control.supported) {
      started = now();
      const view = control.decode(bytes);
      const firstDecodeNs = elapsed(started);
      started = now();
      const value = view.data.metrics.power;
      const firstSparseNs = elapsed(started);
      assert.equal(value, 42);
      native = { supported: true, ...control.metadata, requireNs, firstDecodeNs, firstSparseNs };
    } else native = control;
  }
  output({ node: process.version, requireNs, firstDecodeNs, firstSparseNs, jsonParseSparseNs,
    native, memory: portable.diagnostics(), fixtureSha256: sha(bytes), verifyOnly });
}
function fixtures() {
  const small = { metrics: { power: 42, temperature: 21 }, samples: [1, 2, 3], text: 'sensor' };
  return [
    ['small', small, 1000],
    ['wide', { ...small, fields: Object.fromEntries(Array.from({ length: 200 }, (_, i) => ['field_' + i, { value: i, text: 'sensor' }])) }, 100],
    ['long-string', { ...small, text: 'x'.repeat(65536) }, 50],
    ['array', { ...small, samples: Array.from({ length: 4096 }, (_, i) => i) }, 30],
  ];
}
async function operations() {
  const initial = metadata();
  const portable = require('../portable');
  const { toNative, toJson } = require('../views');
  const native = loadNative();
  const rows = [];
  const repeats = Number(options.repeats || 5);
  const scale = Number(options.scale || 1);
  const fixtureMetadata = [];
  for (const [name, data, baseCount] of fixtures()) {
    const encoded = await portable.encodeMessage(data, ['benchmark']);
    const json = JSON.stringify({ seq: 1, time: '1970-01-01T00:00:00Z', meta: { tags: ['benchmark'] }, data });
    const held = await portable.decodeMessage(encoded, 1n, 0n);
    const heldNative = native.supported ? native.decode(encoded) : null;
    fixtureMetadata.push({ fixture: name, encodedBytes: encoded.length, jsonBytes: Buffer.byteLength(json), encodedSha256: sha(encoded) });
    const lanes = [
      ['portable-cold-sparse', async () => (await portable.decodeMessage(encoded, 1n, 0n)).data.metrics.power, value => assert.equal(value, 42)],
      ['json-cold-sparse', () => JSON.parse(json).data.metrics.power, value => assert.equal(value, 42)],
      ['portable-cold-full', async () => toNative((await portable.decodeMessage(encoded, 1n, 0n)).data), value => assert.deepEqual(value, data)],
      ['json-cold-full', () => JSON.parse(json).data, value => assert.deepEqual(value, data)],
      ['portable-warm-sparse', () => held.data.metrics.power, value => assert.equal(value, 42)],
      ['already-native-warm-sparse', () => data.metrics.power, value => assert.equal(value, 42)],
      ['portable-warm-text', () => held.data.text, value => assert.equal(value, data.text)],
      ['already-native-warm-text', () => data.text, value => assert.equal(value, data.text)],
      ['portable-warm-native-copy', () => toNative(held.data), value => assert.deepEqual(value, data)],
      ['portable-warm-json', () => toJson(held.data), value => assert.deepEqual(JSON.parse(value), data)],
      ['native-data-json-stringify', () => JSON.stringify(data), value => assert.deepEqual(JSON.parse(value), data)],
      ['portable-array-sum', () => { let sum = 0; for (const item of held.data.samples) sum += item; return sum; }, value => assert.equal(value, data.samples.reduce((a, b) => a + b, 0))],
      ['already-native-array-sum', () => { let sum = 0; for (const item of data.samples) sum += item; return sum; }, value => assert.equal(value, data.samples.reduce((a, b) => a + b, 0))],
      ['portable-producer-encoded', () => portable.encodeMessage(data, ['benchmark']), async value => assert.deepEqual(toNative((await portable.decodeMessage(value, 1n, 0n)).data), data)],
      ['json-producer-request', () => Buffer.from(JSON.stringify({ data, tags: ['benchmark'], durability: 'fast' })), value => assert.deepEqual(JSON.parse(value).data, data)],
    ];
    if (native.supported) lanes.push(
      ['native-cold-sparse', () => native.decode(encoded).data.metrics.power, value => assert.equal(value, 42)],
      ['native-cold-full', () => toNative(native.decode(encoded).data), value => assert.deepEqual(value, data)],
      ['native-warm-sparse', () => heldNative.data.metrics.power, value => assert.equal(value, 42)],
      ['native-warm-text', () => heldNative.data.text, value => assert.equal(value, data.text)],
      ['native-warm-native-copy', () => toNative(heldNative.data), value => assert.deepEqual(value, data)],
      ['native-warm-json', () => toJson(heldNative.data), value => assert.deepEqual(JSON.parse(value), data)],
      ['native-array-sum', () => { let sum = 0; for (const item of heldNative.data.samples) sum += item; return sum; }, value => assert.equal(value, data.samples.reduce((a, b) => a + b, 0))],
    );
    for (let repeat = 0; repeat < (verifyOnly ? 1 : repeats + 1); repeat++) {
      const order = repeat % 2 ? [...lanes].reverse() : lanes;
      for (const [lane, operation, check] of order) {
        const count = verifyOnly ? 1 : Math.max(1, Math.round(baseCount * scale));
        const cpu = process.cpuUsage();
        const started = now();
        let last;
        for (let i = 0; i < count; i++) last = await operation();
        const ns = elapsed(started);
        const used = process.cpuUsage(cpu);
        await check(last);
        if (verifyOnly || repeat > 0) rows.push({ fixture: name, lane, repeat,
          operations: count, nsPerOperation: ns === null ? null : ns / count,
          cpuNsPerOperation: verifyOnly ? null : (used.user + used.system) * 1000 / count });
        last = undefined;
      }
      // Let unreachable read owners finalize between repetitions; never collect
      // inside a timed lane or hide retained memory behind forced GC.
      await new Promise(resolve => setImmediate(resolve));
    }
  }
  const after = metadata();
  assert.deepEqual(after.sourceSha256, initial.sourceSha256, 'runtime source changed during capture');
  assert.equal(after.wasmSha256, initial.wasmSha256, 'Wasm changed during capture');
  output({ ...initial, native: native.supported ? native.metadata : native,
    nativeProducer: { supported: false, reason: 'native addon exposes committed appends, not storage-free encoding; compare full producer delivery with the HTTP/DUTYKD harness' },
    fixtures: fixtureMetadata, verifyOnly, rows,
    notes: ['JSON producer writes a different format; report its cost, not an interchangeable codec ratio.',
      'Cold lanes receive already available bytes; they do not include HTTP or storage.',
      'Already-native warm controls exclude construction cost.',
      'All lanes await operation results equally; process startup and fixture preparation stay outside timing.'] });
}
function metadata() {
  const artifact = fs.readFileSync(path.join(packageRoot, 'portable/plasmite_core.wasm'));
  const files = ['portable.js', 'views.js', 'input.js', 'scripts/bench_portable_views.js'];
  return { node: process.version, platform: process.platform, arch: process.arch,
    wasmSha256: sha(artifact), wasmBytes: artifact.length, wasmGzipBytes: zlib.gzipSync(artifact).length,
    sourceSha256: Object.fromEntries(files.map(name => [name, sha(fs.readFileSync(path.join(packageRoot, name)))])),
    coreManifest: JSON.parse(fs.readFileSync(path.join(packageRoot, 'portable/manifest.json'), 'utf8')) };
}
async function startup() {
  const portable = require('../portable');
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-portable-cold-'));
  try {
    const input = fixtures()[0][1];
    const fixture = path.join(directory, 'fixture.lite3'), json = path.join(directory, 'fixture.json');
    fs.writeFileSync(fixture, await portable.encodeMessage(input));
    fs.writeFileSync(json, JSON.stringify({ seq: 1, time: '1970-01-01T00:00:00Z', meta: { tags: [] }, data: input }));
    const rows = [];
    for (let repeat = 0; repeat < Number(options.repeats || 7); repeat++) {
      const args = [__filename, '--worker-cold', '--fixture', fixture, '--json', json];
      if (verifyOnly) args.push('--verify-only');
      if (options['native-addon']) args.push('--native-addon', options['native-addon'], '--library-file', options['library-file']);
      rows.push(JSON.parse(execFileSync(process.execPath, args, { encoding: 'utf8' })));
    }
    output({ ...metadata(), rows, verifyOnly,
      notes: ['Isolated processes; startup excluded, module require/first decode reported separately.',
        'Compressed module bytes describe the added download payload; no simulated network bandwidth claim.'] });
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
}
async function retention() {
  if (!global.gc) throw new Error('retention mode requires node --expose-gc');
  const portable = require('../portable');
  const collectUntil = async expected => {
    for (let i = 0; i < 100; i++) {
      global.gc(); await new Promise(resolve => setImmediate(resolve));
      if (portable.diagnostics().ownerCount === expected) return portable.diagnostics();
    }
    throw new Error('owner finalization did not reach ' + expected);
  };
  const payload = await portable.encodeMessage({ nested: { value: 7 }, text: 'x'.repeat(65536) });
  const before = portable.diagnostics();
  assert.equal(before.ownerCount, 0);
  let message = await portable.decodeMessage(payload, 1n, 0n);
  let child = message.data.nested;
  message = null;
  const retained = await collectUntil(1);
  assert.equal(child.value, 7);
  assert.ok(retained.backingBytes >= payload.length);
  child = null;
  const released = await collectUntil(0);
  let temporary;
  for (let i = 0; i < 50; i++) {
    temporary = await portable.decodeMessage(payload, 1n, 0n);
    assert.equal(temporary.data.nested.value, 7);
    temporary = null;
  }
  const reused = await collectUntil(0);
  output({ ...metadata(), nsPerOperation: null, before, retained, released, reused,
    notes: ['GC/finalization diagnostic, not timing.', 'backingBytes counts owner payload capacities, not metadata/workspace/allocator overhead.',
      'Linear memory stays at its high-water size and can reuse released storage.'] });
}
(async () => {
  if (options['worker-cold']) return cold();
  if (options.mode === 'startup') return startup();
  if (options.mode === 'retention') return retention();
  return operations();
})().catch(error => { console.error(error); process.exitCode = 1; });
