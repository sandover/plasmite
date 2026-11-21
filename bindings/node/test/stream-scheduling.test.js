const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { Worker } = require('node:worker_threads');
const { Client } = require('../index');

async function fixture(run) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-scheduling-'));
  const client = new Client(dir);
  const pool = client.createPool('events', 1048576n);
  try { await run(pool); }
  finally { pool.close(); client.close(); fs.rmSync(dir, { recursive: true, force: true }); }
}

function within(promise, ms) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error('native wait starved unrelated work')), ms);
  })]).finally(() => clearTimeout(timer));
}

test('128 idle local reads leave libuv filesystem work available and close safely', async () => {
  await fixture(async pool => {
    const streams = (await Promise.all(Array.from({ length: 128 }, () => pool.follow()))).map(reader => reader._stream);
    const reads = streams.map(stream => stream.nextJsonWait(5000));
    try {
      await within(fs.promises.stat(__filename), 500);
      streams.forEach(stream => stream.close());
      assert.ok((await within(Promise.all(reads), 500)).every(read => read.status === 3));
    } finally { streams.forEach(stream => stream.close()); await Promise.allSettled(reads); }
  });
});

test('queued local waits keep an absolute budget and preserve their cursor', async () => {
  await fixture(async pool => {
    const streams = (await Promise.all(Array.from({ length: 128 }, () => pool.follow()))).map(reader => reader._stream);
    try {
      const reads = streams.map(stream => stream.nextJsonWait(15));
      assert.ok((await within(Promise.all(reads), 500)).every(read => read.status === 0));
      const message = await pool.feed({ value: 7 });
      for (const stream of streams) {
        const read = await stream.nextJsonWait(0);
        assert.equal(read.status, 1);
        assert.equal(read.view.seq, message.seq);
      }
    } finally { streams.forEach(stream => stream.close()); }
  });
});

test('terminating a worker cancels pending native waits without entering its dead environment', async () => {
  const addon = path.resolve(__dirname, '../index.js');
  for (let i = 0; i < 3; i++) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-wait-worker-'));
    const worker = new Worker(`
      const {parentPort,workerData}=require('node:worker_threads');
      const {Client}=require(workerData.addon);
      const client=new Client(workerData.dir), pool=client.createPool('waiting',1048576n);
      (async () => {
        const reader = await pool.follow();
        reader.next(1000);
        parentPort.postMessage('waiting');
      })();
    `, { eval: true, workerData: { addon, dir } });
    try {
      await within(new Promise((resolve, reject) => { worker.once('message', resolve); worker.once('error', reject); }), 2000);
      await within(worker.terminate(), 2000);
    } finally { await worker.terminate(); fs.rmSync(dir, { recursive: true, force: true }); }
  }
  await within(fs.promises.stat(__filename), 500);
});

test('owned local tails filter before limiting and retain values after close and overwrite', async () => {
  await fixture(async pool => {
    await pool.feed({ nested: { value: 1 } }, { tags: ['ignore'] });
    const kept = await pool.feed({ nested: { value: 2 }, values: [3, 4] }, { tags: ['keep'] });
    const tail = await pool.fetch({ since: 1n, tags: ['keep'] });
    const { value: message } = await tail.next();
    assert.equal(message.seq, kept.seq);
    assert.equal(message.data.nested.value, 2);
    assert.equal(message.data.values[1], 4);
    const child = message.field('nested');
    assert.deepEqual(await tail.next(), { done: true, value: undefined });
    for (let i = 0; i < 300; i++) await pool.feed({ fill: 'x'.repeat(4096) });
    pool.close();
    assert.equal(child.value, 2);
    assert.throws(() => { child.value = 3; }, TypeError);
  });
});

test('owned local iterator return interrupts an idle read and keeps retained values valid', async () => {
  await fixture(async pool => {
    const tail = await pool.follow();
    const pending = tail.next();
    await new Promise(resolve => setTimeout(resolve, 5));
    const returned = tail.return();
    assert.deepEqual(await within(pending, 500), { done: true, value: undefined });
    assert.deepEqual(await within(returned, 500), { done: true, value: undefined });
    const controller = new AbortController();
    const aborting = await pool.follow({ since: 'oldest', signal: controller.signal });
    const read = aborting.next(); controller.abort();
    assert.equal((await within(read, 500)).done, true);
  });
});

test('returning an unused UDP iterator leaves another caller’s read alone', async () => {
  const { UdpFollow } = require('../index');
  let complete;
  let interrupts = 0;
  const follow = new UdpFollow({
    nextEventWait() { return new Promise(resolve => { complete = resolve; }); },
    interruptRead() { interrupts++; complete({ status: 0 }); },
    isFinished: false,
    close() {},
  });
  const pending = follow.nextEvent(5);
  await follow.events().return();
  assert.equal(interrupts, 0);
  complete({ status: 0 });
  assert.equal(await pending, null);
  follow.close();
});

test('UDP loop exit and reader-level abort close the native subscription', async () => {
  const { UdpFollow } = require('../index');
  for (const stop of ['break', 'abort reader', 'abort iterator']) {
    let closes = 0;
    const controller = new AbortController();
    const follow = new UdpFollow({
      async nextEventWait() {
        return { status: 1, event: { kind: 3, startSeq: 1n, nextSeq: 1n, terminal: false } };
      },
      isFinished: false,
      close() { closes++; },
    }, stop === 'abort reader' ? controller.signal : undefined);
    if (stop === 'break') {
      for await (const event of follow.events()) {
        assert.equal(event.kind, 'progress');
        break;
      }
    } else if (stop === 'abort reader') {
      controller.abort();
    } else {
      const iterator = follow.events({ signal: controller.signal });
      assert.equal((await iterator.next()).done, false);
      controller.abort();
      assert.equal(closes, 1);
      await iterator.return();
    }
    assert.equal(closes, 1, stop);
    assert.equal(follow.finished, true);
  }
});

test('a stream reuses one completion resource across reads', async () => {
  const { createHook } = require('node:async_hooks');
  await fixture(async pool => {
    for (let i = 0; i < 3; i++) await pool.feed({ value: i });
    const stream = (await pool.fetch({ since: 1n }))._stream;
    let resources = 0;
    const hook = createHook({ init(id, type) { if (type === 'plasmite:stream') resources++; } }).enable();
    try {
      for (let i = 0; i < 3; i++) assert.equal((await stream.nextJsonWait(1000)).status, 1);
      assert.equal((await stream.nextJsonWait(0)).status, 2);
      assert.equal(resources, 1);
    } finally { hook.disable(); stream.close(); }
  });
});

test('async hooks can close a stream during channel or Promise creation', async () => {
  const { createHook } = require('node:async_hooks');
  await fixture(async pool => {
    for (const kind of ['plasmite:stream', 'PROMISE']) {
      const stream = (await pool.follow())._stream;
      let closed = false;
      const hook = createHook({ init(id, type) {
        if (!closed && type === kind) { closed = true; stream.close(); }
      } }).enable();
      try { assert.equal((await within(stream.nextJsonWait(5000n), 500)).status, 3); assert.equal(closed, true); }
      finally { hook.disable(); stream.close(); }
    }
  });
});

test('reused completion channels preserve each caller’s AsyncLocalStorage context', async () => {
  const { AsyncLocalStorage } = require('node:async_hooks');
  const local = new AsyncLocalStorage();
  await fixture(async pool => {
    await pool.feed({ value: 1 }); await pool.feed({ value: 2 });
    const stream = (await pool.fetch({ since: 1n }))._stream;
    try {
      for (const name of ['first', 'second']) {
        await local.run(name, async () => {
          const result = await stream.nextJsonWait(1000);
          assert.equal(result.status, 1);
          assert.equal(local.getStore(), name);
        });
      }
    } finally { stream.close(); local.disable(); }
  });
});

test('owned tail progress extends past a single per-read timeout interval', async () => {
  await fixture(async pool => {
    const first = setTimeout(() => pool.feed({ number: 1 }), 70);
    const second = setTimeout(() => pool.feed({ number: 2 }), 150);
    try {
      const values = [];
      const reader = await pool.follow();
      try {
        for (let i = 0; i < 2; i++) values.push((await reader.next(0.12)).value.data.number);
      } finally { reader.close(); }
      assert.deepEqual(values, [1, 2]);
    } finally { clearTimeout(first); clearTimeout(second); }
  });
});

test('an owned follow keeps the reader open after a per-read timeout', async () => {
  await fixture(async pool => {
    const reader = await pool.follow();
    try {
      assert.deepEqual(await within(reader.next(0.01), 500), { done: false, value: undefined });
      await pool.feed({ n: 1 });
      assert.equal((await reader.next()).value.data.n, 1);
    } finally { reader.close(); }
  });
});

test('an unlimited owned tail keeps one native read across quiet intervals', async () => {
  await fixture(async pool => {
    const original = pool._inner.openRead.bind(pool._inner);
    let waits = 0;
    pool._inner.openRead = (...args) => {
      const stream = original(...args), wait = stream.nextJsonWait.bind(stream);
      stream.nextJsonWait = budget => { waits++; assert.equal(budget, 18446744073709551615n); return wait(budget); };
      return stream;
    };
    const tail = await pool.follow();
    const next = tail.next();
    const timer = setTimeout(() => pool.feed({ value: 1 }), 25);
    try {
      assert.equal((await within(next, 500)).value.data.value, 1);
      assert.equal(waits, 1);
    } finally { clearTimeout(timer); await tail.return(); }
  });
});

test('an overlapping local next leaves the active read open', async () => {
  const { ErrorKind } = require('../index');
  await fixture(async pool => {
    const reader = await pool.follow();
    const pending = reader.next();
    try {
      await assert.rejects(reader.next(0), { kind: ErrorKind.Busy });
      const written = await pool.feed({ value: 1 });
      assert.equal((await within(pending, 500)).value.seq, written.seq);
    } finally {
      await reader.return();
      await pending;
    }
  });
});

test('UDP nextEvent takes milliseconds while iterator next takes seconds', async () => {
  const { UdpFollow } = require('../index');
  const timeouts = [];
  const follow = new UdpFollow({
    async nextEventWait(timeout) {
      timeouts.push(timeout);
      return { status: 0 };
    },
    isFinished: false,
    close() {},
  });
  try {
    assert.equal(await follow.nextEvent(500), null);
    assert.deepEqual(await follow.next(0.5), { done: false, value: undefined });
    assert.deepEqual(timeouts, [500n, 500n]);
  } finally {
    follow.close();
  }
});
