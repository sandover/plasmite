/*
Purpose: Provide an HTTP/JSON RemoteClient for the Node binding.
Key Exports: RemoteClient, RemotePool, RemoteError.
Role: JavaScript client for Plasmite HTTP API 2.0.
Invariants: Uses the JSON and binary streams defined in spec/v2.
Invariants: Base URL must be http(s) without a path.
Invariants: Readers expose async iteration and propagate terminal records.
*/

const { Message, messageFromEnvelope, parseMessageJson, normalizeMessageSeq, timeText, parsePoolInfo } = require("./message");
const { ERROR_KIND_VALUES, mapDurability, mapErrorKind } = require("./mappings");
const { readStart, normalizeWait } = require("./follow");
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
    this.missing = error?.missing === undefined ? undefined : normalizeMessageSeq(error.missing);
    this.nextSeq = error?.next_seq === undefined ? undefined : normalizeMessageSeq(error.next_seq);
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
  async createPool(pool, sizeBytes = 1024 * 1024) {
    const payload = { pool, size_bytes: Number(sizeBytes) };
    const url = buildUrl(this.baseUrl, ["v2", "pools"]);
    await this._requestJson("POST", url, payload, parsePoolInfo);
    return new RemotePool(this, pool);
  }

  /**
   * Open a remote pool handle.
   * @param {string} pool
   * @returns {Promise<RemotePool>}
   */
  async openPool(pool, { create = false } = {}) {
    const payload = { create };
    const url = buildUrl(this.baseUrl, ["v2", "pools", pool, "open"]);
    await this._requestJson("POST", url, payload);
    return new RemotePool(this, pool);
  }

  /**
   * Fetch pool metadata.
   * @param {string} pool
   * @returns {Promise<unknown>}
   */
  async poolInfo(pool) {
    const url = buildUrl(this.baseUrl, ["v2", "pools", pool]);
    const data = await this._requestJson("GET", url, null, parsePoolInfo);
    return data.pool;
  }

  /**
   * List pools on the remote server.
   * @returns {Promise<unknown[]>}
   */
  async listPools() {
    const url = buildUrl(this.baseUrl, ["v2", "pools"]);
    const data = await this._requestJson("GET", url, null, parsePoolInfo);
    return data.pools;
  }

  /**
   * Delete a pool by name.
   * @param {string} pool
   * @returns {Promise<void>}
   */
  async deletePool(pool) {
    const url = buildUrl(this.baseUrl, ["v2", "pools", pool]);
    await this._requestJson("DELETE", url, null);
  }

  async _requestJson(method, url, body, decode = text => parseMessageJson(text, "message")) {
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
    return decode(await response.text());
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
      if (response.status === 404 && url.pathname.startsWith("/v2/") &&
          response.headers.get("plasmite-version") !== "2") {
        await response.body?.cancel().catch(() => {});
        throw new RemoteError({ kind: "Usage", message: "Server requires upgrade to Plasmite 2" }, 404);
      }
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

  info() {
    return this.client.poolInfo(this.pool);
  }

  async feed(data, { tags, durability = "fast" } = {}) {
    if (data instanceof Message) {
      if (tags !== undefined) throw new RemoteError({ kind: "Usage", message: "cannot supply tags when forwarding a message" }, 400);
      tags = data.tags;
      data = data.data;
    }
    if (!data || typeof data !== "object" || Array.isArray(data)) {
      throw new RemoteError({ kind: "Usage", message: "feed requires an object" }, 400);
    }
    const url = buildUrl(this.client.baseUrl, ["v2", "pools", this.pool, "feed"]);
    const payload = { data, tags: tags ?? [], durability: mapDurability(durability) };
    const response = await this.client._requestJson("POST", url, payload);
    return messageFromEnvelope(response.message);
  }

  follow(options = {}) {
    return openV2Reader(this, true, options);
  }

  followLite3(options = {}) {
    return openV2Reader(this, true, options, "lite3");
  }

  fetch(options = {}) {
    return openV2Reader(this, false, options);
  }

  fetchLite3(options = {}) {
    return openV2Reader(this, false, options, "lite3");
  }

  followView(options = {}) {
    return openV2Reader(this, true, options, "view");
  }

  fetchView(options = {}) {
    return openV2Reader(this, false, options, "view");
  }

  async feedLite3(payload, { tags, durability = "fast" } = {}) {
    if (tags !== undefined) throw new RemoteError({ kind: "Usage", message: "cannot supply tags when forwarding a message" }, 400);
    if (!Buffer.isBuffer(payload)) throw new TypeError("Lite3 payload must be a Buffer");
    if (payload.length > MAX_FRAME_BYTES) throw new RangeError("Lite3 payload exceeds maximum frame size");
    const response = await this._feedBinary(payload, durability, "application/json");
    const message = parseMessageJson((await readBody(response)).toString("utf8"), "message").message;
    return messageFromEnvelope(message);
  }

  async _feedBinary(payload, durability, accept) {
    const url = buildUrl(this.client.baseUrl, ["v2", "pools", this.pool, "feed"]);
    const policy = mapDurability(durability);
    if (policy !== "fast") url.searchParams.set("durability", policy);
    return this.client._request("POST", url, { body: payload, contentType: LITE3_TYPE, accept });
  }

  async feedView(data, { tags, durability = "fast" } = {}) {
    if (tags !== undefined && (data instanceof Message || require("./views").messageReference(data))) {
      throw new RemoteError({ kind: "Usage", message: "cannot supply tags when forwarding a message" }, 400);
    }
    if (data instanceof Message) {
      tags = data.tags;
      data = data.data;
    }
    const backend = require("./portable");
    const prepared = backend.prepareAppend(data, tags);
    try {
      const response = await this._feedBinary(prepared.payload, durability, "application/x-plasmite-identity");
      const seq = frameHeader(response.headers.get("plasmite-seq"), "plasmite-seq");
      const time = frameHeader(response.headers.get("plasmite-timestamp-ns"), "plasmite-timestamp-ns");
      return backend.messageWithIdentity(prepared.message, seq, time);
    } catch (err) {
      prepared.discard();
      throw err;
    }
  }

}

async function openV2Reader(pool, live, options = {}, representation = "json") {
  const { since, last, tags, signal, ...extra } = options;
  if (Object.keys(extra).length) {
    throw new RemoteError({ kind: "Usage", message: "read options are since, last, tags and signal" }, 400);
  }
  let kind, value;
  try { [kind, value] = readStart(since, last); } catch (err) {
    throw new RemoteError({ kind: "Usage", message: err.message }, 400);
  }
  const route = live ? "follow" : "fetch";
  const base = buildUrl(pool.client.baseUrl, ["v2", "pools", pool.pool, route]);
  for (const tag of tags ?? []) base.searchParams.append("tag", tag);

  const initial = new URL(base);
  switch (kind) {
    case 1: initial.searchParams.set("since", "oldest"); break;
    case 2: initial.searchParams.set("since", value.toString()); break;
    case 3: initial.searchParams.set("since", timeText(value)); break;
    case 4: initial.searchParams.set("since", `${value}ns`); break;
    case 5: initial.searchParams.set("last", value.toString()); break;
  }
  const request = requestController(signal);
  const accept = representation === "json" ? "application/jsonl" : "application/x-plasmite-lite3-stream";
  const open = url => pool.client._requestStream(url, request.controller, accept);
  let response;
  try {
    response = await open(initial);
  } catch (err) {
    request.dispose();
    throw err;
  }

  const invalid = message => new RemoteError({ kind: "Io", message }, 0);
  const incarnation = response.headers.get("plasmite-incarnation");
  const read = async function* () {
    let lastMessage;
    let continuation;
    while (!request.controller.signal.aborted) {
      if (!response.body) throw invalid("read response has no body");
      let cutoff;
      for await (const record of responseRecords(response.body, representation, invalid)) {
        if (record.type === "complete") {
          if (live) throw invalid("unexpected completion of live read");
          return;
        }
        if (record.type === "error") throw new RemoteError(record, response.status);
        if (record.type === "cutoff") {
          if (live || record.until === undefined) throw invalid("invalid stream cutoff");
          cutoff = record;
          break;
        }
        if (lastMessage !== undefined && record.seq <= lastMessage) throw invalid("read sequence did not advance");
        lastMessage = record.seq;
        yield record;
      }
      if (!cutoff) throw invalid("read response ended without a control record");
      const next = normalizeMessageSeq(cutoff.next_seq);
      const until = normalizeMessageSeq(cutoff.until);
      if (next === 0n || (lastMessage !== undefined && next <= lastMessage)
          || next > until
          || (continuation && (until !== continuation.until || next < continuation.next))) {
        throw invalid("invalid stream cutoff");
      }
      continuation = { next, until };
      const resumed = new URL(base);
      resumed.searchParams.set("since", next.toString());
      resumed.searchParams.set("until", until.toString());
      response = await open(resumed);
      const current = response.headers.get("plasmite-incarnation");
      if (incarnation === null || current === null) throw invalid("missing pool identity on stream continuation");
      if (current !== incarnation) {
        throw new RemoteError({ kind: "NotFound", message: "pool was replaced during the read" }, 404);
      }
    }
  };

  const iterator = read();
  let pending;
  let reading = false;
  let shutdown;
  const finish = { done: true, value: undefined };
  const close = () => {
    if (shutdown) return shutdown;
    request.dispose();
    const cancelBody = response.body?.locked ? undefined : response.body?.cancel();
    shutdown = Promise.allSettled([cancelBody, iterator.return()]);
    return shutdown;
  };
  return {
    async next(wait) {
      if (shutdown || request.controller.signal.aborted) {
        await close();
        return finish;
      }
      if (wait !== undefined) {
        try { wait = normalizeWait(wait); } catch (err) {
          throw new RemoteError({ kind: "Usage", message: err.message }, 400);
        }
      }
      if (reading) throw invalid("stream already has an active reader");
      reading = true;
      const item = pending ??= iterator.next();
      // A timed-out call leaves one pull pending for the next caller.
      item.catch(() => {});
      let timer;
      try {
        const result = wait === undefined ? await item : await Promise.race([item,
          new Promise(resolve => { timer = setTimeout(() => resolve(null), Math.min(wait, 2147483647)); })]);
        if (result === null) return { done: false, value: undefined };
        pending = undefined;
        if (result.done) await close();
        return result;
      } catch (err) {
        const aborted = request.controller.signal.aborted;
        await close();
        if (aborted) return finish;
        if (err instanceof RemoteError) throw err;
        throw invalid(err.message);
      } finally {
        reading = false;
        clearTimeout(timer);
      }
    },
    close() { void close(); },
    async return() { await close(); return finish; },
    [Symbol.asyncIterator]() { return this; },
  };
}

async function* responseRecords(body, representation, invalid) {
  const input = body.getReader();
  try {
    if (representation === "json") {
      const decoder = new TextDecoder();
      let remainder = "";
      for (;;) {
        const { done, value } = await input.read();
        const lines = (remainder + decoder.decode(value, { stream: !done })).split("\n");
        remainder = lines.pop();
        if (done) lines.push(remainder);
        for (let line of lines) {
          if (!line.trim()) continue;
          if (line.endsWith("\r")) line = line.slice(0, -1);
          const value = parseMessageJson(line);
          yield value.type === undefined ? messageFromEnvelope(value, Buffer.from(line, "utf8")) : value;
        }
        if (done) return;
      }
    }

    const source = new FrameReader(input);
    for (;;) {
      const header = await source.read(5, true);
      if (header === null) return;
      const kind = header[0];
      const length = header.readUInt32BE(1);
      if (kind === 0x43 && length > 0 && length <= MAX_FRAME_BYTES) {
        const control = parseMessageJson((await source.read(length)).toString("utf8"));
        if (!["complete", "error", "cutoff"].includes(control.type)) throw invalid("invalid binary control");
        yield control;
      } else if (kind === 0x4d && length >= 16 && length <= MAX_FRAME_BYTES + 16) {
        const identity = await source.read(16);
        const seq = identity.readBigUInt64BE(0);
        const time = identity.readBigUInt64BE(8);
        const payload = await source.read(length - 16, false, representation === "lite3");
        if (representation === "view") {
          yield await require("./portable").decodeMessage(payload, seq, time);
        } else {
          yield { seq, time, payload };
        }
      } else {
        throw invalid("invalid binary read record");
      }
    }
  } finally {
    await input.cancel().catch(() => {});
    input.releaseLock();
  }
}

function requestController(signal) {
  const controller = new AbortController();
  const abort = () => controller.abort(signal.reason);
  signal?.addEventListener("abort", abort, { once: true });
  if (signal?.aborted) abort();
  return { controller, dispose() { signal?.removeEventListener("abort", abort); controller.abort(); } };
}

function frameHeader(value, name) {
  if (value === null || !/^[0-9]{1,20}$/.test(value)) throw new Error(`missing or invalid ${name} header`);
  return normalizeMessageSeq(BigInt(value));
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
