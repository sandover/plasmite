// Diagnostic-only sampled stages and addon Rust allocation requests.
const fs = require('node:fs'), path = require('node:path'), os = require('node:os');
const { performance } = require('node:perf_hooks');
const root = path.resolve(process.argv[2]);
const { Client } = require(root);
const { inputHelpers } = require(path.join(root, 'input'));
const native = require(path.join(root, `native/${process.platform}-${process.arch}/index.node`));
const fixtures = {
  small: { metrics: { power: 42 }, samples: Array.from({ length: 4 }, (_, value) => ({ value })) },
  wide: { metrics: { power: 42 }, samples: Array.from({ length: 128 }, (_, value) => ({ value })),
    ...Object.fromEntries(Array.from({ length: 256 }, (_, i) => ['field' + i, 'x'.repeat(64)])) },
  text64k: { metrics: { power: 42 }, samples: [{ value: 1 }], text: 'x'.repeat(65536) },
  array: { metrics: { power: 42 }, samples: Array.from({ length: 4096 }, (_, value) => ({ value })) },
};
async function main() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-input-profile-'));
  const client = new Client(dir), pool = client.createPool('input', 32 * 1024 * 1024);
  const measurements = [];
  try {
    for (const [fixture, data] of Object.entries(fixtures)) {
      for (let i = 0; i < 30; i++) await pool.feedView(data);
      const rows = [];
      for (let repeat = 0; repeat < 5; repeat++) {
        const target = pool._inner.prepareAppend(0);
        native.resetInputProfile();
        const start = performance.now();
        const pending = target.encode(data, [], inputHelpers);
        const encodingMs = performance.now() - start;
        const profile = native.inputProfile();
        await pending;
        rows.push({ encodingMs, profile });
      }
      measurements.push({ fixture, rows });
    }
    process.stdout.write(JSON.stringify({ node: process.version, measurements,
      stages: ['classify', 'protocol-property', 'own-keys', 'UTF8', 'builder-put', 'typeof', 'field/key-get'],
      limit: 'One in64 calls sampled; clock/counter overhead remains. The final row counts addon Rust allocator requests, not V8/C++ or separately linked core allocations.' }) + '\n');
  } finally { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
