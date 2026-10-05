const test = require("node:test");
const assert = require("node:assert/strict");
const { RemoteClient, RemoteError } = require("../remote");
const { mapDurability } = require("../mappings");

async function withFetchStub(fetchImpl, callback) {
  const originalFetch = global.fetch;
  const calls = [];
  global.fetch = async (url, options) => {
    calls.push({ url, options });
    return fetchImpl(url, options);
  };
  try {
    await callback(calls);
  } finally {
    global.fetch = originalFetch;
  }
}

test("error and durability mappings reject unknown or inherited values", () => {
  for (const kind of ["toString", "constructor", 999]) {
    assert.equal(new RemoteError({ error: { kind } }, 500).kind, 8);
  }
  assert.equal(new RemoteError({ error: { kind: "RetentionGap" } }, 500).kind, 9);
  assert.throws(() => mapDurability("constructor"), /durability must be/);
  assert.equal(mapDurability(1), "flush");
});

test("bearer credentials never go to a non-loopback HTTP URL", async () => {
  await withFetchStub(() => {
    assert.fail("fetch must not run for a token sent over remote HTTP");
  }, async (calls) => {
    const client = new RemoteClient("http://service.example:9700", { token: "secret" });
    await assert.rejects(client.listPools(), /bearer tokens require HTTPS outside loopback/);
    assert.equal(calls.length, 0);

    const changed = new RemoteClient("https://service.example:9700").withToken("secret");
    changed.baseUrl = new URL("http://service.example:9700");
    await assert.rejects(changed.listPools(), /bearer tokens require HTTPS outside loopback/);
    assert.equal(calls.length, 0);
  });
});

test("HTTP remains available without credentials and with loopback credentials", async () => {
  await withFetchStub(() => new Response('{"pools":[]}', {
    headers: { "Content-Type": "application/json" },
  }), async (calls) => {
    const remote = new RemoteClient("http://service.example:9700");
    assert.deepEqual(await remote.listPools(), []);

    for (const host of ["localhost", "127.0.0.1", "127.12.34.56", "[::1]"]) {
      const local = new RemoteClient(`http://${host}:9700`, { token: "local-token" });
      assert.deepEqual(await local.listPools(), []);
      assert.equal(calls.at(-1).options.headers.Authorization, "Bearer local-token");
    }
    assert.equal(calls.length, 5);
  });
});

test("HTTPS bearer requests use the requested origin and reject redirects", async () => {
  await withFetchStub((url, options) => {
    assert.equal(options.redirect, "manual");
    assert.equal(options.headers.Authorization, "Bearer secret");
    return new Response(null, {
      status: 302,
      headers: { Location: "http://attacker.example/collect" },
    });
  }, async (calls) => {
    const client = new RemoteClient("https://service.example:9700", { token: "secret" });
    await assert.rejects(client.listPools(), /redirected the request \(HTTP 302\); redirects are not followed/);
    assert.equal(calls.length, 1);
  });
});

test("stream requests also enforce transport security and reject redirects", async () => {
  await withFetchStub(() => new Response(null, {
    status: 307,
    headers: { Location: "https://other.example/stream" },
  }), async (calls) => {
    const controller = new AbortController();
    const remote = new RemoteClient("https://service.example:9700", { token: "secret" });
    await assert.rejects(
      remote._requestStream(new URL("https://service.example:9700/v0/pools/p/tail"), controller),
      /redirected the request \(HTTP 307\); redirects are not followed/,
    );
    assert.equal(calls[0].options.redirect, "manual");
    assert.equal(calls[0].options.headers.Authorization, "Bearer secret");
  });

  await withFetchStub(() => assert.fail("fetch must not run for an unsafe stream URL"), async (calls) => {
    const remote = new RemoteClient("https://service.example:9700", { token: "secret" });
    await assert.rejects(
      remote._requestStream(new URL("http://service.example/v0/pools/p/tail"), new AbortController()),
      /bearer tokens require HTTPS outside loopback/,
    );
    assert.equal(calls.length, 0);
  });
});
