const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');
const root = path.resolve(process.env.PLASMITE_HTTP_TEST_BINDING || path.join(__dirname, '..'));
const available = fs.existsSync(path.join(root, 'portable.js'));
const binary = process.env.PLASMITE_BIN || path.resolve(__dirname, '../../../target/debug/plasmite');
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

test('HTTP views preserve exact values, forwarding, transport limits and retained children', {
  skip: !available || !fs.existsSync(binary), timeout: 20000,
}, async t => {
  const { RemoteClient, toNative, toLite3 } = require(root);
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-http-views-'));
  const server = spawn(binary, ['--dir', directory, 'serve', '--bind', '127.0.0.1:0', '--remote-bind', '127.0.0.1:0'], {
    env: { ...process.env, PLASMITE_ACCESS_HOME: path.join(directory, 'access') }, stdio: ['ignore', 'ignore', 'pipe'],
  });
  let stderr = '';
  server.stderr.on('data', value => { stderr += value; });
  t.after(async () => {
    if (server.exitCode === null) {
      const exited = new Promise(resolve => server.once('exit', resolve));
      server.kill(); await exited;
    }
    fs.rmSync(directory, { recursive: true, force: true });
  });
  const bind = path.join(directory, '.plasmite-serve', 'local.json');
  const deadline = Date.now() + 5000;
  while (!fs.existsSync(bind)) {
    assert.ok(server.exitCode === null && Date.now() < deadline, stderr || 'server did not start');
    await sleep(10);
  }
  const client = new RemoteClient('http://' + JSON.parse(fs.readFileSync(bind, 'utf8')));
  await client.createPool('views', 1048576);
  const pool = await client.openPool('views');
  const data = { metrics: { power: 42 }, exact: 9223372036854775807n,
    bytes: Buffer.from([0, 255]), zero: -0, samples: [{ value: 1 }], text: '雪\0😀' };
  const pending = pool.appendView(data, ['source', '\0']);
  data.metrics.power = 99; data.bytes[0] = 9;
  const first = await pending;
  assert.equal(first.data.metrics.power, 42);
  assert.equal(first.data.bytes[0], 0);
  assert.equal(first.data.exact, 9223372036854775807n);
  assert.ok(Object.is(first.data.zero, -0));
  assert.ok(first.timestampNs > 0n);
  const encoded = toLite3(first);
  encoded.fill(0);
  assert.equal(first.data.exact, 9223372036854775807n);
  assert.throws(() => toLite3(first.data), /message view/);
  const copied = toNative(first.data);
  copied.samples[0].value = 99;
  assert.equal(first.data.samples[0].value, 1);
  const forwarded = await pool.appendView(first);
  assert.deepEqual(toNative(forwarded.meta.tags), ['source', '\0']);
  assert.deepEqual(toNative(forwarded.data), toNative(first.data));
  const retagged = await pool.appendView(first, []);
  assert.deepEqual(toNative(retagged.meta.tags), []);
  const fetched = await pool.getView(first.seq);
  assert.equal(fetched.seq, first.seq);
  assert.equal(fetched.timestampNs, first.timestampNs);
  assert.deepEqual(toNative(fetched.data), toNative(first.data));
  const observed = [];
  for await (const message of pool.tailViews({ sinceSeq: first.seq, maxMessages: 3, timeoutMs: 1000 })) observed.push(message);
  assert.deepEqual(observed.map(value => value.seq), [first.seq, forwarded.seq, retagged.seq]);
  await assert.rejects(async () => { for await (const _ of pool.tailViews({ errorOnGap: true })) {} }, /terminal retention gaps/);

  // An older server ignores the new response type. Use its one JSON receipt;
  // never retry an append or round away its nanosecond identity.
  const fetch = global.fetch;
  global.fetch = (url, options = {}) => fetch(url, {
    ...options, headers: { ...options.headers, Accept: 'application/json' },
  });
  let legacyReceipt;
  try { legacyReceipt = await pool.appendView(first); }
  finally { global.fetch = fetch; }
  const legacyRead = await pool.getView(legacyReceipt.seq);
  assert.equal(legacyReceipt.timestampNs, legacyRead.timestampNs);
  assert.deepEqual(toNative(legacyReceipt.data), toNative(first.data));
  // A failed append frees only its unpublished encoding. Caller snapshots live on.
  const portable = require(path.join(root, 'portable'));
  const beforeFailure = portable.diagnostics();
  global.fetch = async () => { throw new Error('deliberate transport failure'); };
  try {
    await assert.rejects(pool.appendView({ value: 1 }), /deliberate transport failure/);
    await assert.rejects(pool.appendView(first), /deliberate transport failure/);
  } finally { global.fetch = fetch; }
  assert.equal(portable.diagnostics().ownerCount, beforeFailure.ownerCount);
  assert.equal(portable.diagnostics().backingBytes, beforeFailure.backingBytes);
  assert.equal(first.data.metrics.power, 42);
  const child = fetched.data.samples;
  await client.deletePool('views');
  assert.equal(child[0].value, 1);
  assert.equal(first.data.exact, 9223372036854775807n);
});
