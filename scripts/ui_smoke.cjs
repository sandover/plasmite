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

const poolExports = `window.testPage = { state, addMessage, flush, appendRows, fromRaw, parseEvents, loadEarlier,
    copy, toLogin, messageLink, boot, followPool, get pending() { return pending; },
  quietRendering() { renderFacts = () => {}; renderRows = () => {}; applyFilter = () => {}; } };`;
const mapExports = 'window.testPage = { state, addMessage, segmentOf, ageGroups, parseEvents };';
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
  env.page.state.info = { bounds: { oldest: 9007199254740990n } };
  return env;
}
const info = (oldest) => ({ ok: true, status: 200, text: async () => `{"pool":{"bounds":{"oldest":${oldest}}}}` });
const stream = (text) => ({ ok: true, status: 200, body: { getReader() {
  let sent = false;
  return { async read() { if (sent) return { done: true }; sent = true; return { value: new TextEncoder().encode(text), done: false }; } };
} } });

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
    if (url.endsWith('/info')) return info('9007199254740991');
    requested = new URL(url, 'http://localhost');
    return stream('event: message\ndata: {"seq":9007199254740993,"data":1}\n\nevent: message\ndata: {"seq":9007199254740994,"data":2}\n\n');
  };
  await env.page.loadEarlier();
  assert.equal(requested.searchParams.get('since_seq'), '9007199254740993');
  assert.equal(requested.searchParams.get('max'), '2');
  assert.equal(requested.searchParams.get('timeout_ms'), '1000');
  assert.equal(requested.searchParams.get('gap_policy'), 'error');
  assert.equal(env.page.state.messages.length, 3);
  assert.equal(env.nodes.get('earlier').disabled, false);
  assert.equal([...env.timers.values()].some((timer) => timer.delay === 5000), false);
});

test('history aborts a stalled server and exposes retry guidance', async () => {
  const env = historyBrowser();
  env.context.fetch = async (url, options) => {
    if (url.endsWith('/info')) return info('9007199254740991');
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
  env.context.fetch = async (url) => url.endsWith('/info') ? info('9007199254740991') :
    stream('event: message\ndata: {"seq":9007199254740993,"data":1}\n\nevent: error\ndata: {"error":{"message":"retention gap"}}\n\n');
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
