// Focused complete-read and warm-view comparisons. Run after building/staging
// matching optimized addon + libplasmite assets, without competing builds.
// node --expose-gc scripts/bench_views.js [iterations=100] [repeats=5] [output]
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { performance } = require('node:perf_hooks');
const bindingRoot = process.env.PLASMITE_BENCH_BINDING ? path.resolve(process.env.PLASMITE_BENCH_BINDING) : path.join(__dirname, '..');
const { Client, toNative, toJson } = require(bindingRoot);
const warmOnly = process.env.PLASMITE_BENCH_WARM_ONLY === '1';
const assert = require('node:assert/strict');
const iterations = Number(process.argv[2] || 100);
const repeats = Number(process.argv[3] || 5);
if (!Number.isSafeInteger(iterations) || iterations <= 0 || !Number.isSafeInteger(repeats) || repeats <= 0) throw new Error('positive integer counts required');
const fixtures = {
  small: { metrics: { power: 42 }, samples: Array.from({length: 4}, (_, value) => ({value})) },
  wide: { metrics: { power: 42 }, samples: Array.from({length: 128}, (_, value) => ({value})),
    ...Object.fromEntries(Array.from({length: 256}, (_, i) => ['field' + i, 'x'.repeat(64)])) },
  array: { metrics: { power: 42 }, samples: Array.from({length: 4096}, (_, value) => ({value})) },
};
const hash = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const measurements = [];
let sink = 0;
function sync(name, n, fn) {
  const start = performance.now();
  for (let i = 0; i < n; i++) sink += fn();
  return { name, operations: n, elapsedMs: performance.now() - start };
}
async function asyncRun(name, n, fn) {
  const start = performance.now();
  for (let i = 0; i < n; i++) sink += await fn();
  return { name, operations: n, elapsedMs: performance.now() - start };
}
const prefix = xs => { let value = 0, count = 0; for (const x of xs) { value += x.value; if (++count === 3) break; } return value; };
const sum = xs => { let value = 0; for (const x of xs) value += x.value; return value; };
async function main() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-view-bench-'));
  const client = new Client(dir), pool = client.createPool('bench', 32 * 1024 * 1024);
  try {
    for (const [fixture, data] of Object.entries(fixtures)) {
      const receipt = pool.append(data);
      const seq = receipt.seq;
      const initialStart = performance.now();
      const view = await pool.getView(seq);
      const initialViewMs = performance.now() - initialStart;
      const native = pool.get(seq);
      assert.deepEqual(toNative(view.data), data);
      assert.deepEqual(JSON.parse(toJson(view.data)), data);
      const expectedSum = data.samples.length * (data.samples.length - 1) / 2;
      assert.equal(sum(view.data.samples), expectedSum);
      // Warm the JIT and native access separately from recorded repeats.
      for (let i = 0; i < 100; i++) sink += view.data.metrics.power + native.data.metrics.power;
      const rows = [];
      const arrayIterations = fixture === 'small' ? 2000 : 100;
      for (let repeat = 0; repeat < repeats; repeat++) {
        // Alternate lane order to reduce systematic thermal/cache bias.
        const cold = [
          ['cold-identity/view', async () => Number((await pool.getView(seq)).seq)],
          ['cold-identity/current-json', async () => Number(pool.get(seq).seq)],
          ['cold-field/view', async () => (await pool.getView(seq)).data.metrics.power],
          ['cold-field/current-json', async () => pool.get(seq).data.metrics.power],
          ['cold-native/view', async () => toNative((await pool.getView(seq)).data).metrics.power],
          ['cold-native/current-json', async () => pool.get(seq).data.metrics.power],
          ['cold-native/json-parse-safe-fixture', async () => JSON.parse(toJson((await pool.getView(seq)).data)).metrics.power],
        ];
        const warm = [
          ['warm-field/view', iterations * 100, () => view.data.metrics.power],
          ['warm-field/native-object', iterations * 100, () => native.data.metrics.power],
          ['warm-array/view', arrayIterations, () => sum(view.data.samples)],
          ['warm-array/native-object', arrayIterations, () => sum(native.data.samples)],
          ['warm-prefix/view', arrayIterations, () => prefix(view.data.samples)],
          ['warm-prefix/native-object', arrayIterations, () => prefix(native.data.samples)],
          ['warm-map/view', arrayIterations, () => view.data.samples.map(x => x.value).length],
          ['warm-map/native-object', arrayIterations, () => native.data.samples.map(x => x.value).length],
          ['warm-copy/view-toNative', iterations, () => toNative(view.data).metrics.power],
          ['warm-copy/native-structuredClone', iterations, () => structuredClone(native.data).metrics.power],
          ['warm-copy/json-parse-safe-fixture', iterations, () => JSON.parse(toJson(view.data)).metrics.power],
          ['warm-json/view-toJson', iterations, () => toJson(view.data).length],
          ['warm-json/native-stringify', iterations, () => JSON.stringify(native.data).length],
        ];
        if (repeat % 2) { cold.reverse(); warm.reverse(); }
        for (const [name, fn] of warmOnly ? [] : cold) rows.push({ repeat, ...await asyncRun(name, iterations, fn) });
        for (const [name, n, fn] of warm) rows.push({ repeat, ...sync(name, n, fn) });
      }
      global.gc?.();
      const before = process.memoryUsage();
      let children = [];
      for (let i = 0; i < 32; i++) children.push((await pool.getView(seq)).data.metrics);
      global.gc?.();
      const retainedChildren = process.memoryUsage();
      assert.equal(children.every(x => x.power === 42), true);
      children = null;
      // Finalizers may need an event-loop turn; this is a sample, not a GC bound.
      await new Promise(resolve => setImmediate(resolve));
      global.gc?.();
      await new Promise(resolve => setImmediate(resolve));
      const releasedChildren = process.memoryUsage();
      measurements.push({ fixture, initialViewMs, arrayIterations, jsonBytes: Buffer.byteLength(JSON.stringify(data)), rows,
        memory: { before, retainedChildren, releasedChildren } });
    }
    const result = { node: process.version, platform: process.platform, arch: process.arch,
      iterations, repeats, warmOnly, gcExposed: !!global.gc, sink, measurements,
      hashes: Object.fromEntries(['scripts/bench_views.js', 'index.js', 'views.js', `native/${process.platform}-${process.arch}/index.node`,
        `native/${process.platform}-${process.arch}/${process.platform === 'darwin' ? 'libplasmite.dylib' : process.platform === 'win32' ? 'plasmite.dll' : 'libplasmite.so'}`]
        .map(file => [file, hash(file === 'scripts/bench_views.js' ? __filename : path.join(bindingRoot, file))])),
      limits: ['Warm native-object controls already own decoded data; they isolate view access overhead.',
        'Cold controls perform the same requested result through the current full JSON API; initialViewMs separately captures startup including first ABI detection.',
        'Heap/RSS samples do not prove prompt GC or a bound on retention.'] };
    const text = JSON.stringify(result, null, 2) + '\n';
    if (process.argv[4]) fs.writeFileSync(process.argv[4], text); else process.stdout.write(text);
  } finally { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
