// Measure compatibility fixes independently of payload traversal.
// node scripts/bench_view_contract.js [binding-path] [output]
const { performance } = require('node:perf_hooks');
const path = require('node:path');
const fs = require('node:fs');
const crypto = require('node:crypto');
const root = path.resolve(process.argv[2] || path.join(__dirname, '..'));
const { createMessageView } = require(path.join(root, 'views'));
const { Client } = require(root);
const os = require('node:os');
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-view-contract-'));
const client = new Client(dir), pool = client.createPool('bench', 65536);
pool.close();
let sink = 0;
const timings = [];
function measure(name, n, fn) {
  for (let i = 0; i < 1000; i++) fn(i);
  const start = performance.now();
  for (let i = 0; i < n; i++) fn(i);
  timings.push({ name, operations: n, elapsedMs: performance.now() - start });
}
async function main() {
  try {
    const views = [0n, 1234567890n, 18446744073709551615n]
      .map(ns => createMessageView({}, 1n, ns));
    for (let repeat = 0; repeat < 7; repeat++) {
      measure('timestamp', 100000, i => { sink += views[i % views.length].timeRfc3339.length; });
      measure('closed-sync', 10000, () => { try { pool.get(1n); } catch (e) { sink += e.message.length; } });
      const start = performance.now();
      for (let i = 0; i < 10000; i++) { try { await pool.getView(1n); } catch (e) { sink += e.message.length; } }
      timings.push({ name: 'closed-async', operations: 10000, elapsedMs: performance.now() - start });
    }
    const hashes = Object.fromEntries(['views.js', 'native/darwin-arm64/index.node', 'native/darwin-arm64/libplasmite.dylib'].map(file => [file, crypto.createHash('sha256').update(fs.readFileSync(path.join(root, file))).digest('hex')]));
    const result = { node: process.version, root, hashes, timings, sink };
    const output = JSON.stringify(result, null, 2) + '\n';
    if (process.argv[3]) fs.writeFileSync(process.argv[3], output);
    else process.stdout.write(output);
  } finally { client.close(); fs.rmSync(dir, { recursive: true, force: true }); }
}
main().catch(e => { console.error(e); process.exitCode = 1; });
