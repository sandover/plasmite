const test = require("node:test");
const assert = require("node:assert/strict");
const { RemoteClient, RemoteError } = require("../remote");
const { mapDurability } = require("../mappings");
const { frame, control } = require("./http_records");
const { encodeMessage } = require("../portable");

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
    assert.equal(url, "https://service.example:9700/pools");
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

test("an unversioned pool 404 uses the server version to distinguish old servers", async () => {
  const body = '{"error":{"kind":"NotFound","message":"missing pool"}}';
  for (const version of [null, "0", "2"]) {
    await withFetchStub(() => new Response(body, {
      status: 404, headers: version ? { "Plasmite-Version": version } : {},
    }), async (calls) => {
      const client = new RemoteClient("http://localhost:9700");
      await assert.rejects(
        client.poolInfo("missing"),
        version === "2" ? /missing pool/ : /requires upgrade to Plasmite 2/,
      );
      assert.equal(new URL(calls[0].url).pathname, "/pools/missing");
    });
  }
});

test("stream requests also enforce transport security and reject redirects", async () => {
  await withFetchStub(() => new Response(null, {
    status: 307,
    headers: { Location: "https://other.example/stream" },
  }), async (calls) => {
    const controller = new AbortController();
    const remote = new RemoteClient("https://service.example:9700", { token: "secret" });
    await assert.rejects(
      remote._requestStream(new URL("https://service.example:9700/v0/pools/p/follow"), controller),
      /redirected the request \(HTTP 307\); redirects are not followed/,
    );
    assert.equal(calls.length, 1);
    assert.equal(calls[0].url, "https://service.example:9700/v0/pools/p/follow");
    assert.equal(calls[0].options.redirect, "manual");
    assert.equal(calls[0].options.headers.Authorization, "Bearer secret");
  });

  await withFetchStub(() => assert.fail("fetch must not run for an unsafe stream URL"), async (calls) => {
    const remote = new RemoteClient("https://service.example:9700", { token: "secret" });
    await assert.rejects(
      remote._requestStream(new URL("http://service.example/v0/pools/p/follow"), new AbortController()),
      /bearer tokens require HTTPS outside loopback/,
    );
    assert.equal(calls.length, 0);
  });
});


test("HTTP errors retain exact uint64 seq and offset tokens", async () => {
  for (const value of ["0", "9007199254740992", "9007199254740993", "18446744073709551615"]) {
    await withFetchStub(() => new Response(`{"error":{"kind":"Corrupt","message":"bad","seq":${value},"offset":${value},"hint":"repair","path":"pool"}}`, { status: 500 }), async () => {
      const client = new RemoteClient("http://localhost:9700");
      await assert.rejects(client.listPools(), (err) => {
        assert.ok(err instanceof RemoteError);
        assert.equal(err.seq, BigInt(value));
        assert.equal(err.offset, BigInt(value));
        assert.equal(err.path, "pool");
        assert.equal(err.hint, "repair");
        return true;
      });
    });
  }
  assert.equal(new RemoteError({ error: { kind: "Io" } }, 500).seq, undefined);
});

test("2.0 fetch opens eagerly and keeps its bound through cutoff", async () => {
  const { RemotePool } = require("../remote");
  const originalFetch = globalThis.fetch;
  const calls = [];
  globalThis.fetch = async url => {
    const request = new URL(url);
    calls.push(request);
    const payload = encodeMessage({}, ["keep"]);
    const message = seq => frame(BigInt(seq), 123456789n, payload);
    const cutoff = control({ type: "cutoff", next_seq: 2, until: 3 });
    const body = [Buffer.concat([message(1), cutoff]), cutoff, Buffer.concat([message(3), control({ type: "complete" })])];
    return new Response(body[calls.length - 1], { headers: { "Plasmite-Incarnation": "original" } });
  };
  try {
    const pool = new RemotePool(new RemoteClient("http://localhost"), "events");
    const read = await pool.fetch({ last: 3, tags: ["keep"] });
    assert.equal(calls.length, 1);
    assert.equal(calls[0].pathname, "/pools/events/fetch");
    assert.equal(calls[0].searchParams.get("last"), "3");
    const first = (await read.next()).value;
    assert.equal(first.seq, 1n);
    assert.equal(first.time % 1_000_000_000n, 123456789n);
    assert.equal((await read.next()).value.seq, 3n);
    assert.equal((await read.next()).done, true);
    assert.equal(calls[1].searchParams.get("since"), "2");
    assert.equal(calls[1].searchParams.get("until"), "3");
    assert.equal(calls[1].searchParams.get("incarnation"), "original");
    assert.equal(calls[1].searchParams.get("tag"), "keep");
    assert.equal(calls[2].href, calls[1].href);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("HTTP continuation rejects another pool or a missing identity before reading records", async () => {
  const { RemotePool } = require("../remote");
  for (const [identity, kind] of [["replacement", "NotFound"], [null, "Io"]]) {
    let calls = 0;
    await withFetchStub(() => {
      if (++calls === 1) {
        return new Response(control({ type: "cutoff", next_seq: 2, until: 3 }), {
          headers: { "Plasmite-Incarnation": "original" },
        });
      }
      return new Response(frame(2n, 1n, encodeMessage({})), {
        headers: identity === null ? {} : { "Plasmite-Incarnation": identity },
      });
    }, async () => {
      const pool = new RemotePool(new RemoteClient("http://localhost"), "events");
      const reader = await pool.fetch();
      await assert.rejects(reader.next(), { kind: new RemoteError({ kind }, 0).kind });
      assert.equal(calls, 2);
    });
  }
});

test("HTTP follow rejects cutoff without opening another request", async () => {
  let calls = 0;
  await withFetchStub(() => {
    calls++;
    return new Response(control({ type: "cutoff", next_seq: 2 }), {
      headers: { "Plasmite-Incarnation": "original" },
    });
  }, async () => {
    const { RemotePool } = require("../remote");
    const pool = new RemotePool(new RemoteClient("http://localhost"), "events");
    const reader = await pool.follow();
    await assert.rejects(reader.next(), /invalid stream cutoff/);
    assert.equal(calls, 1);
  });
});

test("transport failure before response produces a structured Io error", async () => {
  const cause = new TypeError("fetch failed");
  await withFetchStub(() => { throw cause; }, async () => {
    const { RemotePool } = require("../remote");
    const client = new RemoteClient("https://service.example");
    const pool = new RemotePool(client, "events");
    for (const operation of [() => client.listPools(), () => pool.fetch(), () => pool.feed({ x: 1 })]) {
      await assert.rejects(operation(), error => {
        assert.ok(error instanceof RemoteError);
        assert.equal(error.kind, 8);
        assert.equal(error.cause, cause);
        return true;
      });
    }
  });
});
