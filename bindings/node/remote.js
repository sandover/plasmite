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
const { messageFromEnvelope, parseMessageJson, normalizeMessageSeq } = require("./message");
const { ERROR_KIND_VALUES, mapDurability, mapErrorKind } = require("./mappings");
const MAX_FRAME_BYTES = 256 * 1024 * 1024;
const LITE3_TYPE = "application/x-plasmite-lite3";

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
    this.seq = error && error.seq !== undefined ? normalizeMessageSeq(error.seq) : undefined;
    this.offset = error && error.offset !== undefined ? normalizeMessageSeq(error.offset) : undefined;
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
    let payload;
    let contentType;
    if (method !== "GET" && method !== "DELETE") {
      contentType = "application/json";
      payload = JSON.stringify(body);
    }
    const response = await this._request(method, url, { body: payload, contentType });
    if (response.status === 204) {
      return null;
    }
    return parseMessageJson(await response.text(), "message");
  }

  async _request(method, url, { body, contentType, accept = "application/json", signal } = {}) {
    assertSafeTransport(url, this.token);
    const headers = { Accept: accept };
    if (contentType) headers["Content-Type"] = contentType;
    if (this.token) {
      headers.Authorization = `Bearer ${this.token}`;
    }

    const response = await fetch(url.toString(), {
      method,
      headers,
      body,
      signal,
      redirect: "manual",
    });
    rejectRedirect(response);

    if (!response.ok) {
      throw await parseRemoteError(response);
    }

    return response;
  }

  async _requestStream(url, controller, accept = "application/json") {
    return this._request("GET", url, { accept, signal: controller.signal });
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

  /** Encode once and retain the sent data with the server's committed identity. */
  async appendView(data, tags, durability = "fast") {
    const backend = require("./portable");
    const prepared = backend.prepareAppend(data, tags);
    try {
      const url = buildUrl(this.client.baseUrl, ["v0", "pools", this.pool, "append_lite3"]);
      const policy = mapDurability(durability);
      if (policy !== "fast") url.searchParams.set("durability", policy);
      const response = await this.client._request("POST", url, {
        body: prepared.payload, contentType: LITE3_TYPE, accept: "application/x-plasmite-identity",
      });
      let seq, timestampNs;
      if (response.status === 204) {
        seq = frameHeader(response.headers.get("plasmite-seq"), "plasmite-seq");
        timestampNs = frameHeader(response.headers.get("plasmite-timestamp-ns"), "plasmite-timestamp-ns");
      } else {
        // Older v0 servers ignore the new Accept type and return the JSON receipt.
        const receipt = parseMessageJson((await readBody(response)).toString("utf8"), "message").message;
        seq = normalizeMessageSeq(receipt.seq);
        timestampNs = receiptTimestamp(receipt.time);
      }
      return backend.messageWithIdentity(prepared.message, seq, timestampNs);
    } catch (error) {
      prepared.discard();
      throw error;
    }
  }

  async getView(seq, options = {}) {
    const frame = await this.getLite3(seq, options);
    return require("./portable").decodeMessage(frame.payload, frame.seq, frame.timestampNs);
  }

  async *tailViews(options = {}) {
    const backend = require("./portable");
    for await (const frame of this._tailLite3(options, false)) {
      yield await backend.decodeMessage(frame.payload, frame.seq, frame.timestampNs);
    }
  }

  /** Append encoded bytes; current servers return a complete JSON receipt. */
  async appendLite3(payload, durability = "fast") {
    if (!Buffer.isBuffer(payload)) throw new TypeError("Lite3 payload must be a Buffer");
    if (payload.length > MAX_FRAME_BYTES) throw new RangeError("Lite3 payload exceeds maximum frame size");
    const policy = mapDurability(durability);
    const url = buildUrl(this.client.baseUrl, ["v0", "pools", this.pool, "append_lite3"]);
    if (policy !== "fast") url.searchParams.set("durability", policy);
    const response = await this.client._request("POST", url, {
      body: payload, contentType: LITE3_TYPE,
    });
    const bytes = await readBody(response);
    return normalizeMessageSeq(parseMessageJson(bytes.toString("utf8"), "message").message.seq);
  }

  /** Read bytes and their identity from one response on current servers. */
  async getLite3(seq, { signal } = {}) {
    const expected = normalizeMessageSeq(seq);
    const url = buildUrl(this.client.baseUrl, ["v0", "pools", this.pool, "messages", String(expected), "lite3"]);
    const response = await this.client._request("GET", url, { accept: LITE3_TYPE, signal });
    try {
      const actual = frameHeader(response.headers.get("plasmite-seq"), "plasmite-seq");
      const timestampNs = frameHeader(response.headers.get("plasmite-timestamp-ns"), "plasmite-timestamp-ns");
      if (actual !== expected) throw new Error("remote Lite3 frame sequence does not match request");
      return binaryFrame(actual, timestampNs, await readBody(response));
    } catch (error) {
      await response.body?.cancel().catch(() => {});
      throw error;
    }
  }

  /** Stream complete owned frames without building message objects. */
  async *tailLite3(options = {}) {
    yield* this._tailLite3(options, true);
  }

  async *_tailLite3(options, owned) {
    if (options.errorOnGap === true) throw new Error("binary HTTP tails cannot report terminal retention gaps");
    const url = tailUrl(this.client.baseUrl, this.pool, "tail_lite3", options);
    const request = requestController(options.signal);
    let input;
    try {
      const response = await this.client._requestStream(url, request.controller, `${LITE3_TYPE}-stream`);
      if (!response.body) throw new Error("remote Lite3 tail response has no body");
      input = response.body.getReader();
      const source = new FrameReader(input);
      while (true) {
        const header = await source.read(20, true);
        if (header === null) return;
        const length = header.readUInt32BE(16);
        if (length > MAX_FRAME_BYTES) throw new Error("Lite3 frame exceeds maximum response size");
        yield binaryFrame(header.readBigUInt64BE(0), header.readBigUInt64BE(8), await source.read(length, false, owned));
      }
    } finally {
      request.dispose();
      if (input) {
        await input.cancel().catch(() => {});
        input.releaseLock();
      }
    }
  }

  /**
   * Tail remote messages as an async iterable.
   * @param {{sinceSeq?: number|bigint, maxMessages?: number|bigint, timeoutMs?: number, tags?: string[], errorOnGap?: boolean}} [options]
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
    if (options.errorOnGap === true) {
      url.searchParams.set("gap_policy", "error");
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
        const payload = parseMessageJson(line);
        if (payload && payload.error) {
          throw new RemoteError(payload, response.status);
        }
        yield messageFromEnvelope(payload, raw);
      }
    } finally {
      controller.abort();
      reader.close();
    }
  }
}

function tailUrl(baseUrl, pool, route, options) {
  const url = buildUrl(baseUrl, ["v0", "pools", pool, route]);
  if (options.sinceSeq !== undefined) url.searchParams.set("since_seq", String(normalizeMessageSeq(options.sinceSeq)));
  if (options.maxMessages !== undefined) url.searchParams.set("max", String(normalizeMessageSeq(options.maxMessages)));
  if (options.timeoutMs !== undefined) url.searchParams.set("timeout_ms", String(options.timeoutMs));
  for (const tag of options.tags || []) url.searchParams.append("tag", String(tag));
  return url;
}

function requestController(signal) {
  const controller = new AbortController();
  const abort = () => controller.abort(signal.reason);
  signal?.addEventListener("abort", abort, { once: true });
  if (signal?.aborted) abort();
  return { controller, dispose() { signal?.removeEventListener("abort", abort); controller.abort(); } };
}

function binaryFrame(seq, timestampNs, payload) {
  return { seq, timestampNs, payload,
    get time() { return new Date(Number(timestampNs / 1_000_000n)); } };
}

function frameHeader(value, name) {
  if (value === null || !/^[0-9]{1,20}$/.test(value)) throw new Error(`missing or invalid ${name} header`);
  return normalizeMessageSeq(BigInt(value));
}

function receiptTimestamp(value) {
  const match = typeof value === "string" && /^(.*T\d\d:\d\d:\d\d)(?:\.(\d{1,9}))?Z$/.exec(value);
  if (!match) throw new Error("invalid timestamp in legacy append receipt");
  const ms = Date.parse(match[1] + "Z");
  if (!Number.isFinite(ms)) throw new Error("invalid timestamp in legacy append receipt");
  return normalizeMessageSeq(BigInt(ms) * 1000000n + BigInt((match[2] || "").padEnd(9, "0")));
}

// Hold one transport chunk and assemble only a fragmented requested frame.
// Yielding a frame leaves the next read to the consumer, preserving backpressure.
class FrameReader {
  constructor(reader) { this.reader = reader; this.chunk = Buffer.alloc(0); this.offset = 0; }
  async read(length, allowEof = false, owned = false) {
    if (length === 0) return Buffer.alloc(0);
    let output;
    let copied = 0;
    while (copied < length) {
      if (this.offset === this.chunk.length) {
        const { done, value } = await this.reader.read();
        if (done) {
          if (allowEof && copied === 0) return null;
          throw new Error("truncated Lite3 frame");
        }
        this.chunk = Buffer.from(value.buffer, value.byteOffset, value.byteLength);
        this.offset = 0;
        if (this.chunk.length === 0) continue;
      }
      const available = this.chunk.length - this.offset;
      if (copied === 0 && available >= length) {
        const result = this.chunk.subarray(this.offset, this.offset + length);
        this.offset += length;
        return owned ? Buffer.from(result) : result;
      }
      output ||= Buffer.allocUnsafe(length);
      const take = Math.min(available, length - copied);
      this.chunk.copy(output, copied, this.offset, this.offset + take);
      copied += take;
      this.offset += take;
    }
    return output;
  }
}

async function readBody(response) {
  const declared = response.headers.get("content-length");
  if (declared !== null && Number(declared) > MAX_FRAME_BYTES) {
    await response.body?.cancel();
    throw new Error("response exceeds maximum frame size");
  }
  if (!response.body) return Buffer.alloc(0);
  const reader = response.body.getReader();
  let output = Buffer.alloc(0);
  let borrowed = true;
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      if (value.byteLength === 0) continue;
      const next = size + value.byteLength;
      if (next > MAX_FRAME_BYTES) throw new Error("response exceeds maximum frame size");
      const chunk = Buffer.from(value.buffer, value.byteOffset, value.byteLength);
      if (size === 0) {
        output = chunk;
      } else {
        if (borrowed || next > output.length) {
          const capacity = Math.min(MAX_FRAME_BYTES, Math.max(next, Number(declared) || 0, size * 2));
          const grown = Buffer.allocUnsafe(capacity);
          output.copy(grown, 0, 0, size);
          output = grown;
          borrowed = false;
        }
        chunk.copy(output, size);
      }
      size = next;
    }
    return output.subarray(0, size);
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
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

function assertSafeTransport(rawUrl, token) {
  if (!token) return;

  const url = new URL(rawUrl.toString());
  if (url.protocol === "http:" && !isLoopbackHost(url.hostname)) {
    throw new Error("remote bearer tokens require HTTPS outside loopback");
  }
}

function isLoopbackHost(hostname) {
  const host = hostname.toLowerCase();
  return host === "localhost" || host === "[::1]" || /^127(?:\.\d{1,3}){3}$/.test(host);
}

function rejectRedirect(response) {
  if ([301, 302, 303, 307, 308].includes(response.status)) {
    throw new Error(`remote server redirected the request (HTTP ${response.status}); redirects are not followed`);
  }
}

function buildUrl(baseUrl, segments) {
  const url = new URL(baseUrl.toString());
  url.pathname = `/${segments.map((segment) => encodeURIComponent(segment)).join("/")}`;
  return url;
}

async function parseRemoteError(response) {
  let payload = null;
  try {
    payload = parseMessageJson(await response.text(), "error");
  } catch (err) {
    if (!(err instanceof SyntaxError)) throw err;
  }
  return new RemoteError(payload, response.status);
}

module.exports = {
  RemoteClient,
  RemotePool,
  RemoteError,
};
