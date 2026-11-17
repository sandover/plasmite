// Run against a dedicated server/pool directory; never point at a user's pool.
// node scripts/bench_http_views.js BASE_URL OUTPUT [operations=300] [repeats=7]
// PLASMITE_BENCH_BINDING selects a fixed package; RECEIPT_ONLY=1 skips view lanes.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const { performance } = require('node:perf_hooks');
const root = path.resolve(process.env.PLASMITE_BENCH_BINDING || path.join(__dirname, '..'));
const { RemoteClient } = require(path.join(root, 'remote'));
const { toNative } = require(path.join(root, 'views'));
const [baseUrl, output] = process.argv.slice(2);
const operations = Number(process.argv[4] || 300), repeats = Number(process.argv[5] || 7);
assert.ok(baseUrl && output && Number.isSafeInteger(operations) && operations > 0);
assert.ok(Number.isSafeInteger(repeats) && repeats > 0);
const fixtures = {
  small: { metrics: { power: 42 }, samples: [1, 2, 3, 4] },
  wide: { metrics: { power: 42 }, ...Object.fromEntries(Array.from({ length: 256 }, (_, i) => ['field' + i, 'x'.repeat(64)])) },
  text64k: { metrics: { power: 42 }, text: 'x'.repeat(65536) },
  array: { metrics: { power: 42 }, samples: Array.from({ length: 4096 }, (_, value) => ({ value })) },
};
function serverCpuMs() {
  const pid = process.env.PLASMITE_BENCH_SERVER_PID;
  if (!pid) return null;
  assert.match(pid, /^[0-9]+$/);
  if (process.env.PLASMITE_SERVER_CPU_PROBE) {
    return Number(execFileSync(process.env.PLASMITE_SERVER_CPU_PROBE, [pid], { encoding: 'utf8' }).trim()) / 1e6;
  }
  const parts = execFileSync('ps', ['-p', pid, '-o', 'time='], { encoding: 'utf8' }).trim().split(':');
  return parts.reduce((total, part) => total * 60 + Number(part), 0) * 1000;
}
async function measure(name, run) {
  const serverStart = serverCpuMs(), cpu = process.cpuUsage(), start = performance.now();
  for (let i = 0; i < operations; i++) assert.equal(await run(), 42);
  const elapsedMs = performance.now() - start, used = process.cpuUsage(cpu), serverEnd = serverCpuMs();
  return { name, operations, elapsedMs, clientCpuUs: used.user + used.system,
    serverCpuMs: serverStart === null ? null : serverEnd - serverStart };
}
async function first(read) {
  const reader = await read;
  try { return (await reader.next()).value; }
  finally { await reader.return(); }
}
async function main() {
  const client = new RemoteClient(baseUrl), poolName = 'bench-http-' + crypto.randomBytes(6).toString('hex');
  await client.createPool(poolName, 64 * 1024 * 1024);
  await client.createPool(poolName + '-read', 64 * 1024 * 1024);
  const source = await client.openPool(poolName + '-read');
  const pool = await client.openPool(poolName), measurements = [];
  try {
    for (const [fixture, data] of Object.entries(fixtures)) {
      const prepared = await source.feed(data), frame = await first(source.fetchLite3({ since: prepared.seq }));
      const url = new URL(`/v2/pools/${poolName}/feed`, baseUrl);
      async function appendReceipt(identity) {
        const response = await fetch(url, { method: 'POST', body: frame.payload, headers: {
          'Content-Type': 'application/x-plasmite-lite3',
          Accept: identity ? 'application/x-plasmite-identity' : 'application/json',
        } });
        if (!response.ok) throw new Error(`append HTTP ${response.status}: ${await response.text()}`);
        if (identity) {
          assert.equal(response.status, 204, 'identity receipt requires the new server');
          assert.ok(BigInt(response.headers.get('plasmite-seq')) > 0n);
          assert.ok(BigInt(response.headers.get('plasmite-timestamp-ns')) > 0n);
          assert.equal((await response.arrayBuffer()).byteLength, 0);
          return 42;
        }
        return (await response.json()).message.data.metrics.power;
      }
      const lanes = [
        ['encoded-append/identity', () => appendReceipt(true)],
        ['encoded-append/full-json-receipt', () => appendReceipt(false)],
      ];
      if (process.env.RECEIPT_ONLY !== '1') {
        const sourceView = await first(source.fetchView({ since: prepared.seq }));
        lanes.push(
        ['forward-selected/view', async () => (await pool.feedView(sourceView)).data.metrics.power],
        ['forward-selected/json', async () => (await pool.feed(data)).data.metrics.power],
        ['append-selected/view', async () => (await pool.feedView(data)).data.metrics.power],
        ['append-selected/json', async () => (await pool.feed(data)).data.metrics.power],
        ['get-selected/view', async () => (await first(source.fetchView({ since: prepared.seq }))).data.metrics.power],
        ['get-selected/json', async () => (await first(source.fetch({ since: prepared.seq }))).data.metrics.power],
        ['get-native/view', async () => toNative((await first(source.fetchView({ since: prepared.seq }))).data).metrics.power],
        ['get-native/json', async () => (await first(source.fetch({ since: prepared.seq }))).data.metrics.power],
      );
      }
      for (const [, run] of lanes) for (let i = 0; i < 5; i++) assert.equal(await run(), 42);
      const rows = [];
      for (let repeat = 0; repeat < repeats; repeat++) {
        for (const [name, run] of repeat % 2 ? [...lanes].reverse() : lanes) rows.push({ repeat, ...await measure(name, run) });
      }
      // Verify all data outside selected-field timing, including array/text contents.
      assert.deepEqual((await first(source.fetch({ since: prepared.seq }))).data, data);
      measurements.push({ fixture, encodedBytes: frame.payload.length, rows });
    }
    const hashes = {};
    for (const file of ['remote.js', 'views.js', 'portable.js', 'portable/plasmite_core.wasm', 'portable/manifest.json']) {
      const full = path.join(root, file);
      if (fs.existsSync(full)) hashes[file] = crypto.createHash('sha256').update(fs.readFileSync(full)).digest('hex');
    }
    const probe = process.env.PLASMITE_SERVER_CPU_PROBE;
    if (probe) hashes.serverCpuProbe = crypto.createHash('sha256').update(fs.readFileSync(probe)).digest('hex');
    fs.writeFileSync(output, JSON.stringify({ node: process.version, operations, repeats, measurements, hashes,
      serverCpuProbe: process.env.PLASMITE_SERVER_CPU_PROBE || 'ps time (coarse)',
      limits: ['Sequential loopback HTTP includes fetch/request scheduling.',
        probe ? 'Server CPU uses the native process counter probe, sampled outside client timing.'
          : 'Server CPU uses ps time deltas; short intervals have coarse resolution.',
        'Encoded-append controls isolate receipt work; only full view lanes include native encoding and view acquisition.',
        'JSON fixtures contain safe integers and no native bytes.'] }, null, 2) + '\n');
  } finally { await client.deletePool(poolName); await client.deletePool(poolName + '-read'); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
