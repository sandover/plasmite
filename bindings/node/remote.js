/*
Purpose: Provide an HTTP/JSON RemoteClient for the Node binding.
Key Exports: RemoteClient, RemotePool, RemoteTail, RemoteError.
Role: JS-side remote access that mirrors the v0 server protocol.
Invariants: Uses JSON request/response envelopes from spec/remote/v0.
Invariants: Base URL must be http(s) without a path.
Invariants: Tail streams are JSONL and can be canceled.
*/

const { Readable } = require("node:stream");
const readline = require("node:readline");

class RemoteError extends Error {
  constructor(payload, status) {
    const error = payload && payload.error ? payload.error : payload;
    const message = error && error.message ? error.message : `Remote error ${status}`;
    super(message);
    this.name = "RemoteError";
    this.status = status;
    this.kind = error && error.kind ? error.kind : "Io";
    this.hint = error && error.hint ? error.hint : undefined;
    this.path = error && error.path ? error.path : undefined;
    this.seq = error && error.seq ? error.seq : undefined;
    this.offset = error && error.offset ? error.offset : undefined;
  }
}

class RemoteClient {
  constructor(baseUrl, options = {}) {
    this.baseUrl = normalizeBaseUrl(baseUrl);
    this.token = options.token || null;
  }

  withToken(token) {
    this.token = token;
    return this;
  }

  async createPool(pool, sizeBytes) {
    const payload = { pool, size_bytes: Number(sizeBytes) };
    const url = buildUrl(this.baseUrl, ["v0", "pools"]);
    const data = await this._requestJson("POST", url, payload);
    return data.pool;
  }

  async openPool(pool) {
    const payload = { pool };
    const url = buildUrl(this.baseUrl, ["v0", "pools", "open"]);
    await this._requestJson("POST", url, payload);
    return new RemotePool(this, pool);
  }

  async poolInfo(pool) {
    const url = buildUrl(this.baseUrl, ["v0", "pools", pool, "info"]);
    const data = await this._requestJson("GET", url, null);
    return data.pool;
  }

  async listPools() {
    const url = buildUrl(this.baseUrl, ["v0", "pools"]);
    const data = await this._requestJson("GET", url, null);
    return data.pools;
  }

  async deletePool(pool) {
    const url = buildUrl(this.baseUrl, ["v0", "pools", pool]);
    await this._requestJson("DELETE", url, null);
  }

  async _requestJson(method, url, body) {
    const headers = { Accept: "application/json" };
    if (this.token) {
      headers.Authorization = `Bearer ${this.token}`;
    }
    let payload;
    if (method !== "GET" && method !== "DELETE") {
      headers["Content-Type"] = "application/json";
      payload = JSON.stringify(body);
    }

    const response = await fetch(url.toString(), {
      method,
      headers,
      body: payload,
    });

    if (!response.ok) {
      throw await parseRemoteError(response);
    }

    if (response.status === 204) {
      return null;
    }
    return response.json();
  }

  async _requestStream(url, controller) {
    const headers = { Accept: "application/json" };
    if (this.token) {
      headers.Authorization = `Bearer ${this.token}`;
    }

    const response = await fetch(url.toString(), {
      method: "GET",
      headers,
      signal: controller.signal,
    });

    if (!response.ok) {
      throw await parseRemoteError(response);
    }

    return response;
  }
}

class RemotePool {
  constructor(client, pool) {
    this.client = client;
    this.pool = pool;
  }

  poolRef() {
    return this.pool;
  }

  async append(data, descrips = [], durability = "fast") {
    const payload = { data, descrips, durability };
    const url = buildUrl(this.client.baseUrl, ["v0", "pools", this.pool, "append"]);
    const response = await this.client._requestJson("POST", url, payload);
    return response.message;
  }

  async get(seq) {
    const url = buildUrl(this.client.baseUrl, [
      "v0",
      "pools",
      this.pool,
      "messages",
      String(seq),
    ]);
    const response = await this.client._requestJson("GET", url, null);
    return response.message;
  }

  async tail(options = {}) {
    const url = buildUrl(this.client.baseUrl, ["v0", "pools", this.pool, "tail"]);
    if (options.sinceSeq !== undefined) {
      url.searchParams.set("since_seq", String(options.sinceSeq));
    }
    if (options.maxMessages !== undefined) {
      url.searchParams.set("max", String(options.maxMessages));
    }
    if (options.timeoutMs !== undefined) {
      url.searchParams.set("timeout_ms", String(options.timeoutMs));
    }

    const controller = new AbortController();
    const response = await this.client._requestStream(url, controller);
    return new RemoteTail(response, controller);
  }
}

class RemoteTail {
  constructor(response, controller) {
    if (!response.body) {
      throw new Error("remote tail response has no body");
    }
    this.controller = controller;
    const stream = Readable.fromWeb(response.body);
    this.reader = readline.createInterface({ input: stream, crlfDelay: Infinity });
    this.iterator = this.reader[Symbol.asyncIterator]();
    this.done = false;
  }

  async next() {
    if (this.done) {
      return null;
    }
    const { value, done } = await this.iterator.next();
    if (done) {
      this.done = true;
      return null;
    }
    if (!value || !value.trim()) {
      return this.next();
    }
    return JSON.parse(value);
  }

  cancel() {
    if (this.done) {
      return;
    }
    this.done = true;
    this.controller.abort();
    this.reader.close();
  }
}

function normalizeBaseUrl(raw) {
  const url = new URL(raw);
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("remote base URL must use http or https");
  }
  if (url.pathname && url.pathname !== "/") {
    throw new Error("remote base URL must not include a path");
  }
  url.pathname = "/";
  url.search = "";
  url.hash = "";
  return url;
}

function buildUrl(baseUrl, segments) {
  const url = new URL(baseUrl.toString());
  url.pathname = `/${segments.map((segment) => encodeURIComponent(segment)).join("/")}`;
  return url;
}

async function parseRemoteError(response) {
  let payload = null;
  try {
    payload = await response.json();
  } catch (err) {
    payload = null;
  }
  return new RemoteError(payload, response.status);
}

module.exports = {
  RemoteClient,
  RemotePool,
  RemoteTail,
  RemoteError,
};
