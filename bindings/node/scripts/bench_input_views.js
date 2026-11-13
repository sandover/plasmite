// Run with matching optimized addon/core assets and no competing CPU work.
// node --expose-gc scripts/bench_input_views.js [iterations=200] [repeats=5] [output]
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const { performance, PerformanceObserver, monitorEventLoopDelay } = require('node:perf_hooks');
const root = path.resolve(process.env.PLASMITE_BENCH_BINDING || path.join(__dirname, '..'));
const { Client, Durability, toNative, toJson } = require(root);
const { createNativeMessage, createMessageView, messageReference, valueReference } = require(path.join(root, 'views'));
const iterations = Number(process.argv[2] || 200), repeats = Number(process.argv[3] || 5);
if (!Number.isSafeInteger(iterations) || iterations <= 0 || !Number.isSafeInteger(repeats) || repeats <= 0) throw new Error('positive counts required');
const fixtures = {
  small: { metrics: { power: 42 }, samples: Array.from({ length: 4 }, (_, value) => ({ value })) },
  wide: { metrics: { power: 42 }, samples: Array.from({ length: 128 }, (_, value) => ({ value })),
    ...Object.fromEntries(Array.from({ length: 256 }, (_, i) => ['field' + i, 'x'.repeat(64)])) },
  text64k: { metrics: { power: 42 }, samples: [{ value: 1 }], text: 'x'.repeat(65536) },
  array: { metrics: { power: 42 }, samples: Array.from({ length: 4096 }, (_, value) => ({ value })) },
};
let sink = 0, gcEvents = [];
const observer = new PerformanceObserver(list => { gcEvents.push(...list.getEntries().map(e => ({ start: e.startTime, duration: e.duration }))); });
observer.observe({ entryTypes: ['gc'] });
const pause = () => new Promise(resolve => setImmediate(resolve));
function jsonAppend(pool, text, tags = []) {
  const target = pool._inner.prepareAppend(Durability.Fast);
  return target.json(text, tags).then(createNativeMessage);
}
async function measure(name, n, fn) {
  global.gc?.();
  await pause();
  const lag = monitorEventLoopDelay({ resolution: 1 });
  lag.enable();
  await new Promise(resolve => setTimeout(resolve, 2));
  lag.reset();
  const cpu = process.cpuUsage(), start = performance.now();
  for (let i = 0; i < n; i++) sink += await fn();
  const elapsedMs = performance.now() - start, usedCpu = process.cpuUsage(cpu);
  await new Promise(resolve => setTimeout(resolve, 2));
  lag.disable();
  await pause();
  const gc = gcEvents.filter(e => e.start >= start && e.start <= start + elapsedMs);
  return { name, operations: n, elapsedMs, cpuUs: usedCpu.user + usedCpu.system,
    eventLoopMaxMs: lag.max / 1e6, eventLoopP99Ms: lag.percentile(99) / 1e6,
    gcCount: gc.length, gcMs: gc.reduce((n, e) => n + e.duration, 0) };
}
async function retention(pool, data) {
  global.gc?.(); await pause();
  const before = process.memoryUsage();
  let original = await pool.appendView(data);
  let backend = valueReference(original.data).backend.owner;
  const payload = { encodedBytes: backend.byteLength(), reportedBackingBytes: backend.retainedBytes() };
  backend = null;
  let views = [];
  for (let i = 0; i < 16; i++) views.push(await pool.appendView(original));
  original = null;
  global.gc?.(); await pause();
  const retained = process.memoryUsage();
  assert.equal(views.every(view => view.data.metrics.power === 42), true);
  views = null;
  for (let i = 0; i < 3; i++) { global.gc?.(); await pause(); }
  return { payload, before, retained, released: process.memoryUsage() };
}
async function main() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-node-producer-bench-'));
  const client = new Client(dir), pool = client.createPool('input', 64 * 1024 * 1024), destination = client.createPool('output', 64 * 1024 * 1024);
  const consumer = client.openPool('input');
  const measurements = [];
  try {
    for (const [fixture, data] of Object.entries(fixtures)) {
      const source = await pool.appendView(data, ['source']);
      assert.deepEqual(toNative(source.data), data);
      assert.deepEqual(toNative((await jsonAppend(pool, JSON.stringify(data))).data), data);
      const sourceJson = toJson(source.data);
      const n = fixture === 'array' ? Math.max(10, Math.floor(iterations / 4)) : iterations;
      const lanes = [
        ['append-field/native', async () => (await pool.appendView(data)).data.metrics.power],
        ['append-field/stringify-json', async () => (await jsonAppend(pool, JSON.stringify(data))).data.metrics.power],
        ['append-field/legacy-message', async () => pool.append(data).data.metrics.power],
        ['append-native-copy/native', async () => toNative((await pool.appendView(data)).data).metrics.power],
        ['append-native-copy/stringify-json', async () => toNative((await jsonAppend(pool, JSON.stringify(data))).data).metrics.power],
        ['append-consumer-field/native', async () => (await consumer.getView((await pool.appendView(data)).seq)).data.metrics.power],
        ['append-consumer-field/stringify-json', async () => (await consumer.getView((await jsonAppend(pool, JSON.stringify(data))).seq)).data.metrics.power],
        ['forward-field/native', async () => (await destination.appendView(source)).data.metrics.power],
        ['forward-field/explicit-json', async () => (await jsonAppend(destination, toJson(source.data), ['source'])).data.metrics.power],
        ['retag-field/native', async () => (await destination.appendView(source, ['changed'])).data.metrics.power],
        ['retag-field/explicit-json', async () => (await jsonAppend(destination, toJson(source.data), ['changed'])).data.metrics.power],
        ['subtree-length/native', async () => (await destination.appendView({ subset: source.data.samples })).data.subset.length],
        ['subtree-length/explicit-json', async () => (await jsonAppend(destination, '{"subset":' + toJson(source.data.samples) + '}')).data.subset.length],
      ];
      const sourceBackend = messageReference(source).backend;
      if (sourceBackend.owner.encodedBytes) {
        // Stand in for a portable checked owner without requiring Wasm startup.
        // Cold includes export + checked native import; warm reuses that proof.
        const foreign = () => {
          const backend = { ...sourceBackend, encodedBytes: () => sourceBackend.owner.encodedBytes() };
          delete backend.owner;
          return createMessageView(backend, source.seq, source.timestampNs);
        };
        const retained = foreign();
        lanes.push(
          ['foreign-forward/cold-import', async () => (await destination.appendView(foreign())).data.metrics.power],
          ['foreign-forward/warm-import', async () => (await destination.appendView(retained)).data.metrics.power],
        );
      }
      for (const [, fn] of lanes) for (let i = 0; i < 20; i++) sink += await fn();
      const rows = [];
      for (let repeat = 0; repeat < repeats; repeat++) {
        for (const [name, fn] of repeat % 2 ? [...lanes].reverse() : lanes) rows.push({ repeat, ...await measure(name, n, fn) });
      }
      measurements.push({ fixture, jsonBytes: Buffer.byteLength(sourceJson), rows, retention: await retention(destination, data) });
    }
    const exactJson = '{"value":9223372036854775807}', exactData = { value: 9223372036854775807n };
    assert.equal((await pool.appendView(exactData)).data.value, exactData.value);
    assert.equal((await jsonAppend(pool, exactJson)).data.value, exactData.value);
    const exact = [];
    for (let repeat = 0; repeat < repeats; repeat++) {
      exact.push({ repeat, ...await measure('exact/native', iterations, async () => (await pool.appendView(exactData)).data.value === exactData.value ? 1 : 0) });
      exact.push({ repeat, ...await measure('exact/explicit-json-text', iterations, async () => (await jsonAppend(pool, exactJson)).data.value === exactData.value ? 1 : 0) });
    }
    const byteData = { bytes: Buffer.alloc(65536, 7) };
    assert.equal((await pool.appendView(byteData)).data.bytes.length, 65536);
    const nativeBytes = await measure('native-bytes/no-json-equivalent', iterations, async () => (await pool.appendView(byteData)).data.bytes.length);
    const files = ['index.js', 'views.js', 'input.js', `native/${process.platform}-${process.arch}/index.node`,
      `native/${process.platform}-${process.arch}/${process.platform === 'darwin' ? 'libplasmite.dylib' : process.platform === 'win32' ? 'plasmite.dll' : 'libplasmite.so'}`];
    const result = { node: process.version, platform: process.platform, arch: process.arch, iterations, repeats,
      sink, measurements, exact, nativeBytes, hashes: Object.fromEntries(files.map(file => [file,
        crypto.createHash('sha256').update(fs.readFileSync(path.join(root, file))).digest('hex')])),
      limits: ['All JSON comparisons use safe finite numeric fixtures without bytes and return owned views.',
        'The legacy Message lane decodes the complete message synchronously; it has a different interface cost.',
        'The consumer lane uses a separately opened handle in the same process; it does not measure cross-process wakeups.',
        'Explicit JSON conversions operate directly on views, without an intermediate native tree.',
        'GC/heap/RSS samples are observational, not exact allocation or peak-memory counts. Reported backing bytes fall back to encoded length on older libraries.'] };
    const text = JSON.stringify(result, null, 2) + '\n';
    if (process.argv[4]) fs.writeFileSync(process.argv[4], text); else process.stdout.write(text);
  } finally { observer.disconnect(); consumer.close(); pool.close(); destination.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
