/*
Purpose: Provide an HTTP/JSON RemoteClient for the Node binding.
Key Exports: RemoteClient, RemotePool, RemoteError.
Role: JS-side remote access that mirrors the v0 server protocol.
Invariants: Uses JSON request/response envelopes from spec/remote/v0.
Invariants: Base URL must be http(s) without a path.
Invariants: Tail streams are JSONL and exposed as async iterables.
*/

const { Readable } = require("node:stream");
const readline = require("node:readline");
const { messageFromEnvelope } = require("./message");
const { ERROR_KIND_VALUES, mapDurability, mapErrorKind } = require("./mappings");

class RemoteError extends Error {
  /**
   * Build a structured remote error from an error envelope.
   * @param {unknown} payload
   * @param {number} status
   * @returns {RemoteError}
   */
  constructor(payload, status) {
    const error = payload && payload.error ? payload.error : payload;
    const message = error && error.message ? error.message : `Remote error ${status}`;
    super(message);
    this.name = "RemoteError";
    this.status = status;
    this.kind = mapErrorKind(error && error.kind, ERROR_KIND_VALUES.Io);
    this.hint = error && error.hint ? error.hint : undefined;
    this.path = error && error.path ? error.path : undefined;
    this.seq = error && error.seq ? error.seq : undefined;
    this.offset = error && error.offset ? error.offset : undefined;
  }
}

class RemoteClient {
  /**
   * Create a remote client bound to a base URL.
   * @param {string} baseUrl
   * @param {{token?: string}} [options]
   * @returns {RemoteClient}
   */
  constructor(baseUrl, options = {}) {
    this.baseUrl = normalizeBaseUrl(baseUrl);
    this.token = options.token || null;
  }

  /**
   * Set bearer token and return the same client.
   * @param {string} token
   * @returns {RemoteClient}
   */
  withToken(token) {
    this.token = token;
    return this;
  }

  /**
   * Create a pool on the remote server.
   * @param {string} pool
   * @param {number|bigint} sizeBytes
   * @returns {Promise<unknown>}
   */
  async createPool(pool, sizeBytes) {
    const payload = { pool, size_bytes: Number(sizeBytes) };
    const url = buildUrl(this.baseUrl, ["v0", "pools"]);
    const data = await this._requestJson("POST", url, payload);
    return data.pool;
  }

  /**
   * Open a remote pool handle.
   * @param {string} pool
   * @returns {Promise<RemotePool>}
   */
  async openPool(pool) {
    const payload = { pool };
    const url = buildUrl(this.baseUrl, ["v0", "pools", "open"]);
    await this._requestJson("POST", url, payload);
    return new RemotePool(this, pool);
  }

  /**
   * Fetch pool metadata.
   * @param {string} pool
   * @returns {Promise<unknown>}
   */
  async poolInfo(pool) {
    const url = buildUrl(this.baseUrl, ["v0", "pools", pool, "info"]);
    const data = await this._requestJson("GET", url, null);
    return data.pool;
  }

  /**
   * List pools on the remote server.
   * @returns {Promise<unknown[]>}
   */
  async listPools() {
    const url = buildUrl(this.baseUrl, ["v0", "pools"]);
    const data = await this._requestJson("GET", url, null);
    return data.pools;
  }

  /**
   * Delete a pool by name.
   * @param {string} pool
   * @returns {Promise<void>}
   */
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
  /**
   * @param {RemoteClient} client
   * @param {string} pool
   * @returns {RemotePool}
   */
  constructor(client, pool) {
    this.client = client;
    this.pool = pool;
  }

  /**
   * Return pool reference string.
   * @returns {string}
   */
  poolRef() {
    return this.pool;
  }

  /**
   * Append message data to the remote pool.
   * @param {unknown} data
   * @param {string[]} [tags]
   * @param {number|string} [durability]
   * @returns {Promise<import("./message").Message>}
   */
  async append(data, tags = [], durability = "fast") {
    const payload = { data, tags, durability: mapDurability(durability) };
    const url = buildUrl(this.client.baseUrl, ["v0", "pools", this.pool, "append"]);
    const response = await this.client._requestJson("POST", url, payload);
    return messageFromEnvelope(response.message);
  }

  /**
   * Get message by sequence from remote pool.
   * @param {number|bigint} seq
   * @returns {Promise<import("./message").Message>}
   */
  async get(seq) {
    const url = buildUrl(this.client.baseUrl, [
      "v0",
      "pools",
      this.pool,
      "messages",
      String(seq),
    ]);
    const response = await this.client._requestJson("GET", url, null);
    return messageFromEnvelope(response.message);
  }

  /**
   * Tail remote messages as an async iterable.
   * @param {{sinceSeq?: number|bigint, maxMessages?: number|bigint, timeoutMs?: number, tags?: string[]}} [options]
   * @returns {AsyncGenerator<import("./message").Message, void, unknown>}
   */
  async *tail(options = {}) {
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
    if (options.tags !== undefined) {
      const tags = Array.isArray(options.tags) ? options.tags : [options.tags];
      for (const tag of tags) {
        url.searchParams.append("tag", String(tag));
      }
    }

    const controller = new AbortController();
    const response = await this.client._requestStream(url, controller);
    if (!response.body) {
      throw new Error("remote tail response has no body");
    }
    const stream = Readable.fromWeb(response.body);
    const reader = readline.createInterface({ input: stream, crlfDelay: Infinity });
    try {
      for await (const line of reader) {
        if (!line || !line.trim()) {
          continue;
        }
        const raw = Buffer.from(line, "utf8");
        yield messageFromEnvelope(JSON.parse(line), raw);
      }
    } finally {
      controller.abort();
      reader.close();
    }
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
  RemoteError,
};
