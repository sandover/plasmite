// Use a diagnostic-only addon exposing inputScopeCount; no production counter.
const fs = require('node:fs'), path = require('node:path'), os = require('node:os');
const root = path.resolve(process.argv[2]);
const { Client } = require(root);
const native = require(path.join(root, `native/${process.platform}-${process.arch}/index.node`));
const fixtures = {
  small: { metrics: { power: 42 }, samples: Array.from({ length: 4 }, (_, value) => ({ value })) },
  wide: { metrics: { power: 42 }, samples: Array.from({ length: 128 }, (_, value) => ({ value })),
    ...Object.fromEntries(Array.from({ length: 256 }, (_, i) => ['field' + i, 'x'.repeat(64)])) },
  text64k: { metrics: { power: 42 }, samples: [{ value: 1 }], text: 'x'.repeat(65536) },
  array: { metrics: { power: 42 }, samples: Array.from({ length: 4096 }, (_, value) => ({ value })) },
};
async function main() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-scope-count-'));
  const client = new Client(dir), pool = client.createPool('input', 8 * 1024 * 1024);
  try {
    const counts = {};
    for (const [fixture, value] of Object.entries(fixtures)) {
      const before = native.inputScopeCount();
      await pool.feed(value);
      counts[fixture] = native.inputScopeCount() - before;
    }
    process.stdout.write(JSON.stringify({ node: process.version, counts }) + '\n');
  } finally { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
