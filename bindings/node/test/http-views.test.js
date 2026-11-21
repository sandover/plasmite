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
  const { RemoteClient, ErrorKind, Message, toNative } = require(root);
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
  await client.createPool('views', {
    sizeBytes: 1048576, about: 'Readings', example: { file_size: 11, bounds: { oldest: 1 } },
  });
  const pool = await client.openPool('views');
  assert.equal((await pool.info()).about, 'Readings');
  assert.deepEqual((await client.listPools())[0].example, { file_size: 11, bounds: { oldest: 1 } });
  await pool.setDescription({ about: 'Kitchen readings' });
  assert.equal((await pool.info()).about, 'Kitchen readings');
  assert.equal((await pool.info()).example, undefined);
  const data = { metrics: { power: 42 }, exact: 9223372036854775807n,
    bytes: Buffer.from([0, 255]), zero: -0, samples: [{ value: 1 }], text: '雪\0😀' };
  const pending = pool.feed(data, { tags: ['source', '\0'] });
  data.metrics.power = 99; data.bytes[0] = 9;
  const first = await pending;
  assert.ok(first instanceof Message);
  assert.equal(first.data.metrics.power, 42);
  assert.equal(first.data.bytes[0], 0);
  assert.equal(first.data.exact, 9223372036854775807n);
  assert.ok(Object.is(first.data.zero, -0));
  assert.ok(first.time > 0n);
  const encoded = first.raw;
  encoded.fill(0);
  assert.equal(first.data.exact, 9223372036854775807n);
  const copied = toNative(first.data);
  copied.samples[0].value = 99;
  assert.equal(first.data.samples[0].value, 1);
  const forwarded = await pool.feed(first);
  assert.deepEqual(toNative(forwarded.meta.tags), ['source', '\0']);
  assert.deepEqual(toNative(forwarded.data), toNative(first.data));
  await assert.rejects(pool.feed(first, { tags: [] }), err => err.kind === ErrorKind.Usage);
  const retagged = await pool.feed(first.data, { tags: [] });
  assert.deepEqual(toNative(retagged.meta.tags), []);
  const reader = await pool.fetch({ since: first.seq });
  const { value: fetched } = await reader.next();
  await reader.return();
  assert.equal(fetched.seq, first.seq);
  assert.equal(fetched.time, first.time);
  assert.deepEqual(toNative(fetched.data), toNative(first.data));
  const observed = [];
  for await (const message of await pool.fetch({ since: first.seq })) observed.push(message);
  assert.deepEqual(observed.map(value => value.seq), [first.seq, forwarded.seq, retagged.seq]);
  const forwardedMessage = await pool.feed(first);
  assert.deepEqual(forwardedMessage.tags, ['source', '\0']);
  assert.ok(forwardedMessage.seq > retagged.seq);
  await assert.rejects(pool.feed(first, { tags: [] }), err => err.kind === ErrorKind.Usage);
  const fetch = global.fetch;
  // A failed feed frees its unpublished encoding. Caller snapshots live on.
  const portable = require(path.join(root, 'portable'));
  const beforeFailure = portable.diagnostics();
  global.fetch = async () => { throw new Error('deliberate transport failure'); };
  try {
    await assert.rejects(pool.feed({ value: 1 }), /deliberate transport failure/);
    await assert.rejects(pool.feed(first), /deliberate transport failure/);
  } finally { global.fetch = fetch; }
  assert.equal(portable.diagnostics().ownerCount, beforeFailure.ownerCount);
  assert.equal(portable.diagnostics().backingBytes, beforeFailure.backingBytes);
  assert.equal(first.data.metrics.power, 42);
  const json = await pool.feed(Buffer.from('{"maximum":9223372036854775807}'), {
    format: 'json', tags: ['exact'],
  });
  assert.ok(json instanceof Message);
  assert.equal(json.data.maximum, 9223372036854775807n);
  assert.deepEqual(json.tags, ['exact']);
  const child = fetched.data.samples;
  await client.deletePool('views');
  assert.equal(child[0].value, 1);
  assert.equal(first.data.exact, 9223372036854775807n);
});
