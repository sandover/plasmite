// Isolate shared parsed-JSON safety tracking from append/dispatch costs.
// node --expose-gc scripts/bench_json_result_copy.js BINDING_PATH [output]
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const { performance } = require('node:perf_hooks');
const root = path.resolve(process.argv[2] || path.join(__dirname, '..'));
const { Client, toNative } = require(root);
const { createNativeMessage, valueReference } = require(path.join(root, 'views'));
const fixtures = {
  small: { metrics: { power: 42 }, samples: [{ value: 1 }] },
  wide: { metrics: { power: 42 }, samples: Array.from({ length: 128 }, (_, value) => ({ value })),
    ...Object.fromEntries(Array.from({ length: 256 }, (_, i) => ['field' + i, 'x'.repeat(64)])) },
  array: { metrics: { power: 42 }, samples: Array.from({ length: 4096 }, (_, value) => ({ value })) },
};
let sink = 0;
async function main() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-json-result-bench-'));
  const client = new Client(dir), pool = client.createPool('input', 32 * 1024 * 1024);
  const measurements = [];
  try {
    for (const [fixture, data] of Object.entries(fixtures)) {
      const owner = await pool._inner.prepareAppend(0).json(JSON.stringify(data), []);
      const view = createNativeMessage(owner), ref = valueReference(view.data);
      let parseCalls = 0;
      assert.deepEqual(owner.materialize(ref.node, text => { parseCalls++; return JSON.parse(text); }), data);
      for (let i = 0; i < 20; i++) sink += toNative(view.data).metrics.power;
      const iterations = fixture === 'small' ? 5000 : fixture === 'wide' ? 500 : 100;
      const rows = [];
      for (let repeat = 0; repeat < 7; repeat++) {
        global.gc?.();
        const start = performance.now();
        for (let i = 0; i < iterations; i++) sink += toNative(view.data).metrics.power;
        rows.push({ repeat, operations: iterations, elapsedMs: performance.now() - start });
      }
      measurements.push({ fixture, parseCalls, encodedBytes: owner.byteLength(),
        reportedBackingBytes: owner.retainedBytes(), rows });
    }
    const owner = await pool._inner.prepareAppend(0).json('{"value":9223372036854775807}', []);
    const exact = createNativeMessage(owner), ref = valueReference(exact.data);
    let exactParseCalls = 0;
    assert.equal(owner.materialize(ref.node, text => { exactParseCalls++; return JSON.parse(text); }).value, 9223372036854775807n);
    assert.equal(exactParseCalls, 0);
    const library = `native/${process.platform}-${process.arch}/${process.platform === 'darwin' ? 'libplasmite.dylib' : process.platform === 'win32' ? 'plasmite.dll' : 'libplasmite.so'}`;
    const result = { node: process.version, root, sink, measurements, exactParseCalls,
      hashes: Object.fromEntries(['index.js', 'views.js', `native/${process.platform}-${process.arch}/index.node`, library].map(file => [file,
        crypto.createHash('sha256').update(fs.readFileSync(path.join(root, file))).digest('hex')])) };
    const text = JSON.stringify(result, null, 2) + '\n';
    if (process.argv[3]) fs.writeFileSync(process.argv[3], text); else process.stdout.write(text);
  } finally { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
