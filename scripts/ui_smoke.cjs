// Deterministic browser-script checks, without a browser or third-party packages.
// Run: node --test scripts/ui_smoke.cjs
const assert = require('node:assert/strict');
const test = require('node:test');
const vm = require('node:vm');
const fs = require('node:fs');
const path = require('node:path');
const root = path.resolve(__dirname, '..');

function element() {
  return {
    dataset: new Proxy({}, { set(target, key, value) { target[key] = String(value); return true; } }),
    children: [], listeners: {}, hidden: false, disabled: false, value: '',
    style: { setProperty() {} }, classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
    scrollHeight: 100, scrollTop: 0, clientHeight: 100,
    append(...items) { this.children.push(...items); },
    appendChild(item) { this.children.push(item); return item; },
    replaceChildren(...items) { this.children = items; },
    querySelector() { return element(); }, querySelectorAll() { return []; },
    addEventListener(name, fn) { this.listeners[name] = fn; },
    setAttribute() {}, remove() {}, scrollIntoView() {}, focus() {},
    getContext() { return { measureText() { return { width: 10 }; } }; },
  };
}

function browser(page, exports = '') {
  const nodes = new Map();
  const timers = new Map();
  let timerId = 0;
  const context = vm.createContext({
    console, URL, URLSearchParams, TextDecoder, AbortController, Event,
    document: { head: element(), body: element(), fonts: { ready: Promise.resolve() },
      getElementById(id) { if (!nodes.has(id)) nodes.set(id, element()); return nodes.get(id); },
      createElement: element, createDocumentFragment: element, addEventListener() {}, querySelectorAll() { return []; } },
    location: { origin: 'http://localhost', pathname: '/ui/pools/test', search: '?n=2', hash: '',
      protocol: 'http:', assign(url) { this.assigned = url; } },
    history: { replaceState() {} }, navigator: { clipboard: { writeText: async () => {} } },
    setTimeout(fn, delay) { const id = ++timerId; timers.set(id, { fn, delay }); return id; },
    clearTimeout(id) { timers.delete(id); }, setInterval() {}, requestAnimationFrame() { return 1; },
    addEventListener() {}, getSelection() { return { isCollapsed: true }; },
    fetch() { throw new Error('Unexpected fetch'); },
  });
  context.window = context;
  vm.runInContext(fs.readFileSync(path.join(root, 'ui/common.js'), 'utf8'), context);
  if (page) {
    const source = fs.readFileSync(path.join(root, 'ui', page + '.html'), 'utf8');
    const script = [...source.matchAll(/<script>([\s\S]*?)<\/script>/g)].at(-1)[1];
    vm.runInContext(script.replace(/\n        boot\(\);/, '\n' + exports)
      .replace(/\n        boot\(\)\.catch\([^\n]+/, '\n' + exports), context);
  }
  return { context, nodes, timers, api: context.PlasmiteUI, page: context.testPage };
}

const poolExports = `window.testPage = { state, addMessage, flush, appendRows, fromRaw, loadEarlier,
    copy, toLogin, messageLink, boot, followPool, acceptPool, showMissing, get pending() { return pending; },
  quietRendering() { renderFacts = () => {}; renderRows = () => {}; renderDetail = () => {}; applyFilter = () => {}; } };`;
const mapExports = `window.testPage = { state, addMessage, segmentOf, ageGroups, follow, startList, showPools, startReader, stopReader,
  quietPresentation() {
    sharedInner = () => 1; trackShape = () => ({}); layTrack = () => ({}); topology = () => null;
    updateCards = () => {}; showPeekSummary = () => {}; updateActivityButtons = () => {}; placePeek = () => {}; hover = () => {};
  } };`;
const raw = (api, seq) => api.parseJSON(`{"seq":${seq},"time":"2026-01-01T00:00:00Z","meta":{},"data":{"value":1}}`);

test('all inline scripts compile', () => {
  for (const name of ['index', 'pool', 'map', 'access']) {
    const source = fs.readFileSync(path.join(root, 'ui', name + '.html'), 'utf8');
    for (const match of source.matchAll(/<script>([\s\S]*?)<\/script>/g)) new vm.Script(match[1]);
  }
});

test('access setup binds a selected numeric interface and explains DNS listener scope', () => {
  const env = browser('access', 'window.testPage = { setupCommand, sharedAddress, showSetup };');
  const address = env.page.sharedAddress('node.tail123.ts.net');
  assert.equal(address, 'https://node.tail123.ts.net:9743');
  assert.equal(env.page.setupCommand({ pool_dir: './shared' }, address, '100.101.102.103'),
    'plasmite --dir ./shared serve --remote-bind 100.101.102.103:9743 --shared-address https://node.tail123.ts.net:9743');
  env.page.showSetup({ addresses: [{ host: '100.101.102.103', kind: 'vpn', interface: 'utun5' }] });
  assert.match(env.nodes.get('setup-listener').textContent, /only on 100\.101\.102\.103:9743/);
  env.page.showSetup({ addresses: [{ host: 'node.tail123.ts.net', kind: 'name' }] });
  assert.match(env.nodes.get('setup-listener').textContent, /all IPv4 interfaces/);
  assert.equal(env.page.setupCommand({}, address),
    'plasmite --dir ./shared serve --shared-address https://node.tail123.ts.net:9743');
});

test('access setup preserves explicit ports and quotes shell metacharacters literally', () => {
  const { page } = browser('access', 'window.testPage = { setupCommand, sharedAddress, quoted };');
  assert.equal(page.sharedAddress('https://node.tail123.ts.net:1234/'), 'https://node.tail123.ts.net:1234');
  assert.equal(page.setupCommand({ pool_dir: '/tmp/$(bad)' }, 'https://name;bad:9743'),
    "plasmite --dir '/tmp/$(bad)' serve --shared-address 'https://name;bad:9743'");
  assert.equal(page.quoted("/tmp/owner's pools"), "'/tmp/owner'\\''s pools'");
});

test('protocol integers retain exact IDs, while payload numbers retain JSON semantics', () => {
  const { api } = browser();
  const source = '{"seq":9007199254740993,"data":{"seq":9007199254740993,"bounds":{"oldest":9007199254740993},"text":"\\\"seq\\\": 77"},"meta":{"seq":9007199254740993}}';
  const message = api.parseJSON(source);
  assert.equal(message.seq, 9007199254740993n);
  assert.deepEqual(JSON.parse(JSON.stringify(message.data)), JSON.parse(source).data);
  assert.equal(typeof message.meta.seq, 'number');
  const pool = api.parseJSON('{"pools":[{"bounds":{"oldest":9007199254740993,"newest":18446744073709551615},"ring":{"first":9007199254740993,"frames":[[0,480]]}}]}').pools[0];
  assert.equal(pool.bounds.newest, 18446744073709551615n);
  assert.equal(pool.ring.first, 9007199254740993n);
  assert.equal(pool.ring.frames[0][1], 480);
  assert.equal(api.parseJSON('{"message":{"seq":9007199254740993,"data":1}}').message.seq, message.seq);
  assert.equal(api.parseJSON('{"pool":{"bounds":{"oldest":1,"newest":2}}}').pool.bounds.oldest, 1n);
});

test('parser handles escaped keys, duplicate keys and unrelated key names', () => {
  const { api } = browser();
  assert.equal(api.parseJSON('{"s\\u0065q":9007199254740993}').seq, 9007199254740993n);
  assert.equal(api.parseJSON('{"seq":9007199254740993,"seq":9007199254740995}').seq, 9007199254740995n);
  assert.equal(api.parseJSON('{"pool":{"bounds":{"oldest":1}},"pool":{"bounds":{"oldest":9007199254740995}}}').pool.bounds.oldest, 9007199254740995n);
  assert.equal(api.parseJSON('{"bounds.oldest":9007199254740993}')['bounds.oldest'], 9007199254740992);
  assert.throws(() => api.parseJSON('{"seq":1,'));
  assert.equal(api.parseJSON('null'), null);
});

test('exact JSON copy emits a numeric envelope ID and ordinary payload JSON', () => {
  const { api } = browser();
  const message = raw(api, '18446744073709551615');
  message.data.seq = 42;
  const copied = api.messageJSON(message);
  assert.match(copied, /"seq": 18446744073709551615,/);
  assert.equal(api.parseJSON(copied).seq, message.seq);
  assert.equal(JSON.parse(copied).data.seq, 42);
});

test('links, tail windows and u64 reconnect cursors use full decimal IDs', () => {
  const { api, context } = browser();
  context.location.hash = '#9007199254740993';
  assert.equal(new URL(api.loginURL(), context.location.origin).searchParams.get('next'), '/ui/pools/test?n=2#9007199254740993');
  for (const hash of ['#0', '#-1', '#1e3', '#18446744073709551616', '#bad']) {
    context.location.hash = hash;
    assert.equal(new URL(api.loginURL(), context.location.origin).searchParams.get('next'), '/ui/pools/test?n=2');
  }
  assert.equal(api.tailStart({ oldest: 9007199254740993n, newest: 9007199254741000n }, 2), 9007199254740999n);
  assert.equal(api.tailStart({ oldest: 1n, newest: 2n }, 400), 1n);
  assert.equal(api.cursor(18446744073709551616n), null);
  assert.equal(api.sequence('9'.repeat(100000)), null);
  assert.equal(api.sequence('0'.repeat(100000) + '1'), 1n);
});

test('pool rows and map rows keep adjacent large IDs distinct and advance once', () => {
  const pool = browser('pool', poolExports);
  const a = raw(pool.api, '9007199254740992'), b = raw(pool.api, '9007199254740993');
  pool.page.addMessage(a); pool.page.addMessage(b); pool.page.addMessage(b);
  assert.equal(pool.page.pending.length, 2);
  assert.equal(pool.page.state.nextSeq, 9007199254740994n);
  pool.page.appendRows(element(), pool.page.pending, null);
  assert.equal(pool.page.state.rows.size, 2);
  assert.equal(pool.page.state.rows.get(b.seq).dataset.seq, '9007199254740993');
  assert.match(pool.page.messageLink(b), /#9007199254740993$/);
  const map = browser('map', mapExports);
  map.page.addMessage(a); map.page.addMessage(b); map.page.addMessage(b);
  assert.equal(map.nodes.get('messages').children.length, 2);
  assert.equal(map.page.state.nextSeq, 9007199254740994n);
});

test('map geometry converts only a bounded relative index and exact parity', () => {
  const { page } = browser('map', mapExports);
  const pool = { ring: { first: 9007199254740993n, frames: [[0, 480], [480, 480], [960, 480]] } };
  assert.equal(page.segmentOf(pool, 9007199254740994n, 1).start, 480);
  assert.equal(page.segmentOf(pool, 9007199254740992n, 1), null);
  assert.equal(page.segmentOf(pool, 18446744073709551615n, 1), null);
  assert.equal(page.ageGroups(pool, 1).bands[1].intervals[0].start, 0);
});

test('live readers stop at the u64 limit without reconnecting or duplicating it', async () => {
  const pool = browser('pool', poolExports);
  pool.page.addMessage(raw(pool.api, '18446744073709551615'));
  pool.page.addMessage(raw(pool.api, '18446744073709551615'));
  await pool.page.followPool(); // The fixture fetch throws if the reader reconnects.
  assert.equal(pool.page.pending.length, 1);
  assert.equal(pool.nodes.get('live').textContent, 'sequence limit');
  const map = browser('map', mapExports);
  let stopped = false;
  map.page.state.stopReader = () => { stopped = true; };
  map.page.addMessage(raw(map.api, '18446744073709551615'));
  map.page.addMessage(raw(map.api, '18446744073709551615'));
  assert.equal(stopped, true);
  assert.equal(map.nodes.get('messages').children.length, 1);
  assert.match(map.nodes.get('ui-notice').textContent, /maximum sequence/);
});

test('pool list sorts exact sequence bounds in both directions', () => {
  const { page, api } = browser('index', 'window.testPage = { list, sortedFacts };');
  page.list.pools = api.parseJSON('{"pools":[{"name":"b","bounds":{"newest":9007199254740993}},{"name":"a","bounds":{"newest":9007199254740992}}]}').pools;
  page.list.sort = 'newest'; page.list.desc = false;
  assert.equal(page.sortedFacts()[0].name, 'a');
  page.list.desc = true;
  assert.equal(page.sortedFacts()[0].name, 'b');
});

function historyBrowser() {
  const env = browser('pool', poolExports);
  env.page.quietRendering();
  env.page.state.messages = [env.page.fromRaw(raw(env.api, '9007199254740995'))];
  env.page.state.info = { incarnation: 'current', bounds: { oldest: 9007199254740990n } };
  return env;
}
const info = (oldest, incarnation = 'current', newest = oldest) => ({ ok: true, status: 200,
  text: async () => `{"pool":{"incarnation":"${incarnation}","bounds":{"oldest":${oldest},"newest":${newest}}}}` });
const stream = (...chunks) => ({ ok: true, status: 200, body: { getReader() {
  return { async read() {
    if (!chunks.length) return { done: true };
    return { value: new TextEncoder().encode(chunks.shift()), done: false };
  } };
} } });
const jsonStream = (incarnation, ...chunks) => {
  const response = stream(...chunks);
  response.headers = new Headers({ 'plasmite-incarnation': incarnation });
  response.body.cancel = async () => { response.cancelled = true; };
  return response;
};

test('pool follow resumes at the next exact sequence after a split event and disconnect', async () => {
  const env = browser('pool', poolExports);
  env.page.state.info = { incarnation: 'current' };
  env.page.state.nextSeq = 9007199254740993n;
  const requests = [];
  env.context.fetch = async (url) => {
    requests.push(new URL(url, env.context.location.origin));
    if (requests.length === 1) return jsonStream('current',
      '{"seq":900719925474',
      '0993,"data":{"sample":1}}\r\n');
    return { ok: false, status: 404 };
  };
  const following = env.page.followPool();
  await new Promise(setImmediate);
  assert.equal(env.page.pending[0].seq, 9007199254740993n);
  assert.equal(env.nodes.get('live').textContent, 'reconnecting');
  const [id, retry] = [...env.timers].find(([, timer]) => timer.delay === 1000);
  env.timers.delete(id);
  retry.fn();
  await following;
  assert.deepEqual(requests.map(url => url.searchParams.get('since')),
    ['9007199254740993', '9007199254740994']);
  assert.match(env.nodes.get('empty').textContent, /No pool named/);
});

test('stopping a map reader during its retry wait prevents a new request', async () => {
  const env = browser('map', mapExports);
  let requests = 0;
  let signal;
  const events = [];
  env.context.fetch = async (_, options) => {
    requests++;
    signal = options.signal;
    return stream('event: message\ndata: {"seq":1,"data":{}}\n\n');
  };
  const stop = env.page.follow(() => '/events', event => events.push(event));
  await new Promise(setImmediate);
  assert.deepEqual(events.map(event => event.data.seq), [1n]);
  assert.equal(env.timers.size, 1, 'disconnect scheduled a retry');
  stop();
  for (const timer of env.timers.values()) timer.fn();
  await new Promise(setImmediate);
  assert.equal(signal.aborted, true);
  assert.equal(requests, 1);
});

test('a rejected map stream stops retrying and a later start opens a fresh stream', async () => {
  const env = browser('map', mapExports);
  env.context.document.visibilityState = 'visible';
  const requests = [];
  env.context.fetch = async (url, options) => {
    requests.push([url, options.headers?.Accept]);
    return { ok: false, status: 403 };
  };
  env.page.startList();
  await new Promise(setImmediate);
  assert.equal(env.timers.size, 0);
  env.page.startList();
  await new Promise(setImmediate);
  assert.deepEqual(requests, [['/pools', 'text/event-stream'], ['/pools', 'text/event-stream']]);
});

test('signing out discards a pool list event from the old connection', async () => {
  const env = browser('index', `window.testPage = { list, followPoolList, logout,
    quietRendering() { renderPoolList = () => {}; } };`);
  env.page.quietRendering();
  let finishRead, signal;
  let cancelled = false;
  env.context.fetch = async (_, options) => {
    if (options.method === 'DELETE') return { ok: true };
    signal = options.signal;
    return { ok: true, body: { getReader() {
      return {
        read() { return new Promise(resolve => { finishRead = resolve; }); },
        async cancel() { cancelled = true; },
      };
    } } };
  };
  const following = env.page.followPoolList();
  await new Promise(setImmediate);
  await env.page.logout();
  finishRead({ value: new TextEncoder().encode('event: pools\ndata: {"pools":[{"name":"old"}]}\n\n') });
  await following;
  assert.equal(signal.aborted, true);
  assert.equal(cancelled, true);
  assert.equal(env.page.list.pools, null);
  assert.equal(env.timers.size, 0);
});

test('history refresh prevents a stale-bounds request after overwrite', async () => {
  const env = historyBrowser();
  let calls = 0;
  env.context.fetch = async () => { calls++; return info('9007199254740996'); };
  await env.page.loadEarlier();
  assert.equal(calls, 1);
  assert.equal(env.nodes.get('earlier').disabled, false);
  assert.equal(env.nodes.get('earlier').hidden, true);
  assert.match(env.nodes.get('ui-notice').textContent, /overwritten/);
});

test('history sends bounded exact parameters and inserts a finite earlier batch', async () => {
  const env = historyBrowser();
  let requested;
  env.context.fetch = async (url) => {
    if (url === '/pools/test') return info('9007199254740991');
    requested = new URL(url, 'http://localhost');
    return jsonStream('current', '{"seq":9007199254740993,"data":1}\n{"seq":9007199254740994,"data":2}\n{"type":"complete"}\n');
  };
  await env.page.loadEarlier();
  assert.equal(requested.pathname, '/pools/test/fetch');
  assert.equal(requested.searchParams.get('since'), '9007199254740993');
  assert.equal(requested.searchParams.get('until'), '9007199254740994');
  assert.equal(env.page.state.messages.length, 3);
  assert.equal(env.nodes.get('earlier').disabled, false);
  assert.equal([...env.timers.values()].some((timer) => timer.delay === 5000), false);
});

test('history aborts a stalled server and exposes retry guidance', async () => {
  const env = historyBrowser();
  env.context.fetch = async (url, options) => {
    if (url === '/pools/test') return info('9007199254740991');
    return new Promise((resolve, reject) => options.signal.addEventListener('abort', () => reject(new Error('aborted'))));
  };
  const loading = env.page.loadEarlier();
  await new Promise(setImmediate);
  [...env.timers.values()].find((timer) => timer.delay === 5000).fn();
  await loading;
  assert.equal(env.nodes.get('earlier').disabled, false);
  assert.match(env.nodes.get('ui-notice').textContent, /timed out.*again/);
});

test('history surfaces stream errors and retains a partial page', async () => {
  const env = historyBrowser();
  env.context.fetch = async (url) => url === '/pools/test' ? info('9007199254740991') :
    jsonStream('current', '{"seq":9007199254740993,"data":1}\n{"type":"error","error":{"message":"retention gap"}}\n');
  await env.page.loadEarlier();
  assert.equal(env.page.state.messages.length, 2);
  assert.match(env.nodes.get('ui-notice').textContent, /retention gap.*again/);
});

test('clipboard and session failures surface useful status', async () => {
  const env = browser('pool', poolExports);
  env.context.navigator.clipboard.writeText = async () => { throw new Error('denied'); };
  await env.page.copy(element(), 'message');
  assert.match(env.nodes.get('ui-notice').textContent, /Could not copy/);
  env.context.fetch = async () => { throw new Error('offline'); };
  await env.page.boot();
  assert.match(env.nodes.get('ui-notice').textContent, /check your session.*Reload/);
});

test('the pool page shows Feed only for a session with write access', async () => {
  for (const readOnly of [true, false]) {
    const env = browser('pool', poolExports);
    env.page.quietRendering();
    env.context.fetch = async (url) => {
      if (url === '/browser/session') {
        return { ok: true, json: async () => ({ local: false, read_only: readOnly }) };
      }
      if (url === '/pools/test') return info('1');
      return new Promise(() => {});
    };
    await env.page.boot();
    assert.equal(env.nodes.get('append').hidden, readOnly);
  }
});


test('replacement discards old rows and resumes the new retained history', async () => {
  const env = browser('pool', poolExports);
  env.page.quietRendering();
  const { state } = env.page;
  state.info = { incarnation: 'old', bounds: { oldest: 1n, newest: 100n } };
  env.page.addMessage(raw(env.api, '100'));
  state.selected = { seq: 100n };
  state.target = 100n;
  const changed = env.page.acceptPool({ incarnation: 'new', bounds: { oldest: 1n, newest: 2n } }, false);
  assert.equal(changed, true);
  assert.equal(state.nextSeq, 1n);
  assert.equal(state.selected, null);
  assert.equal(state.target, null);
  assert.equal(env.page.pending.length, 0);
  assert.equal(state.messages.length, 0);
  env.page.addMessage(raw(env.api, '1'));
  env.page.addMessage(raw(env.api, '2'));
  assert.deepEqual(Array.from(env.page.pending, message => message.seq), [1n, 2n]);
  assert.match(env.nodes.get('ui-notice').textContent, /replaced/);
  const generation = state.generation;
  assert.equal(env.page.acceptPool({ incarnation: 'new', bounds: { oldest: 2n, newest: 3n } }, false), false);
  assert.equal(state.generation, generation, 'retention and append preserve the view');
  assert.equal(state.nextSeq, 3n);
});

test('stream incarnation catches replacement after the info request', async () => {
  const env = browser('pool', poolExports);
  env.page.quietRendering();
  env.page.state.info = { incarnation: 'old' };
  env.page.state.nextSeq = 101n;
  const requests = [];
  const stale = jsonStream('new', '{"seq":101,"data":{}}\n');
  env.context.fetch = async url => {
    if (url === '/pools/test') return info(1, 'new', 2);
    requests.push(new URL(url, env.context.location.origin));
    if (requests.length === 1) return stale;
    return jsonStream('new', '{"seq":1,"data":{}}\n{"seq":2,"data":{}}\n');
  };
  const running = env.page.followPool();
  await new Promise(setImmediate);
  assert.equal(stale.cancelled, true, 'old request contributes no messages');
  assert.equal(requests[1].searchParams.get('since'), '1');
  assert.deepEqual(Array.from(env.page.pending, message => message.seq), [1n, 2n]);
  env.page.state.reader.abort();
  await running;
});

test('history from an old incarnation cannot repopulate a reset view', async () => {
  const env = historyBrowser();
  env.page.state.info.incarnation = 'old';
  let release;
  env.context.fetch = async () => new Promise(resolve => { release = resolve; });
  const loading = env.page.loadEarlier();
  env.page.acceptPool({ incarnation: 'new', bounds: { oldest: 1n, newest: 2n } }, false);
  release(info(9007199254740990n));
  await loading;
  assert.equal(env.page.state.info.incarnation, 'new');
  assert.equal(env.page.state.messages.length, 0);
});


test('catalog failures mark retained pools stale and a successful scan clears the error', () => {
  const env = browser('index', `window.testPage = { list, receivePools }; renderPoolList = () => {};`);
  env.page.receivePools({ available: true, pools: [{ name: 'events' }] });
  env.page.receivePools({ available: false, pools: [], error: 'Permission denied' });
  assert.equal(env.page.list.pools[0].name, 'events');
  assert.match(env.page.list.error, /last known pools.*Permission denied/);
  env.page.receivePools({ available: true, pools: [{ name: 'new' }] });
  assert.equal(env.page.list.error, '');
  assert.equal(env.page.list.pools[0].name, 'new');
});


test('map resets a selected same-name replacement and ignores an old reader after cancellation', async () => {
  const env = browser('map', mapExports);
  env.page.quietPresentation();
  env.context.location.href = 'http://localhost/ui/map';
  env.page.state.pools = [{ name: 'test', incarnation: 'old', bounds: { oldest: 1n, newest: 100n } }];
  env.page.state.selected = 'test';
  env.page.state.nextSeq = 101n;
  let aborted = false;
  env.page.state.stopReader = () => { aborted = true; };
  env.context.fetch = async () => ({ ok: false, status: 404 });
  env.page.showPools({ available: true, pools: [{ name: 'test', incarnation: 'new', bounds: { oldest: 1n, newest: 2n } }] });
  assert.equal(aborted, true);
  assert.equal(env.page.state.selected, 'test');
  assert.equal(env.page.state.nextSeq, 1n);
  assert.equal(env.nodes.get('messages').children.length, 0);
  assert.match(env.nodes.get('ui-notice').textContent, /replaced/);
  await new Promise(setImmediate);
  env.page.stopReader();
});

test('map stream-start identity resets an info-to-stream replacement before accepting messages', async () => {
  const env = browser('map', mapExports);
  env.page.quietPresentation();
  env.context.location.href = 'http://localhost/ui/map';
  env.page.state.pools = [{ name: 'test', incarnation: 'old', bounds: { oldest: 1n, newest: 100n } }];
  env.page.state.selected = 'test';
  env.page.state.nextSeq = 101n;
  const requests = [];
  env.context.fetch = async url => {
    requests.push(new URL(url, env.context.location.origin));
    if (requests.length === 1) return jsonStream('new', '{"seq":101,"data":{}}\n');
    return jsonStream('new', '{"seq":1,"data":{}}\n{"seq":2,"data":{}}\n');
  };
  env.page.startReader('test');
  await new Promise(setImmediate);
  assert.equal(env.nodes.get('messages').children.length, 0);
  env.page.showPools({ available: true, pools: [{ name: 'test', incarnation: 'new', bounds: { oldest: 1n, newest: 2n } }] });
  await new Promise(setImmediate);
  assert.equal(requests[1].searchParams.get('since'), '1');
  assert.deepEqual(env.nodes.get('messages').children.map(row => row.dataset.seq), ['1', '2']);
  env.page.stopReader();
});


test('a deleted pool clears old rows and recreation restores the controls', () => {
  const env = browser('pool', poolExports);
  env.page.quietRendering();
  env.page.state.canWrite = true;
  env.page.state.info = { incarnation: 'old' };
  env.page.addMessage(raw(env.api, '100'));
  env.page.showMissing();
  assert.equal(env.page.pending.length, 0);
  assert.equal(env.nodes.get('append').hidden, true);
  env.page.acceptPool({ incarnation: 'new', bounds: { oldest: 1n, newest: 2n } }, false);
  assert.equal(env.nodes.get('append').hidden, false);
  assert.equal(env.nodes.get('log-head').hidden, false);
  assert.equal(env.nodes.get('live').hidden, false);
  assert.equal(env.page.state.nextSeq, 1n);
});
