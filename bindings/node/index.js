/*
Purpose: JavaScript entry point for the Plasmite Node binding.
Key Exports: Client, Pool, Message, Stream, Durability, ErrorKind, replay.
Role: Thin wrapper around the native N-API addon.
Invariants: Exports align with native symbols and v0 API semantics.
Notes: Requires libplasmite to be discoverable at runtime.
*/

const { RemoteClient, RemoteError, RemotePool } = require("./remote");
const { Message, parseMessage } = require("./message");
const { ERROR_KIND_VALUES, mapErrorKind } = require("./mappings");

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const path = require("node:path");
const os = require("node:os");

const PLATFORM_DIRS = Object.freeze({
  linux: Object.freeze({ x64: "linux-x64", arm64: "linux-arm64" }),
  darwin: Object.freeze({ x64: "darwin-x64", arm64: "darwin-arm64" }),
  win32: Object.freeze({ x64: "win32-x64" }),
});

function resolvePlatformDir() {
  const byArch = PLATFORM_DIRS[process.platform];
  if (!byArch) {
    return null;
  }
  return byArch[process.arch] ?? null;
}

function resolveNativeAddonPath() {
  const platformDir = resolvePlatformDir();
  if (!platformDir) {
    return null;
  }
  return path.join(__dirname, "native", platformDir, "index.node");
}

const DEFAULT_POOL_DIR = path.join(os.homedir(), ".plasmite", "pools");
const DEFAULT_POOL_SIZE = 1024 * 1024;
const DEFAULT_POOL_SIZE_BYTES = DEFAULT_POOL_SIZE;
const UINT64_MAX = (1n << 64n) - 1n;
let native = null;
let nativeLoadError = null;
let nativeAddonPath = null;

try {
  nativeAddonPath = resolveNativeAddonPath();
  if (nativeAddonPath) {
    native = require(nativeAddonPath);
  } else {
    nativeLoadError = new Error(`unsupported platform: ${process.platform}-${process.arch}`);
  }
} catch (err) {
  nativeLoadError = err;
}

const Durability = native ? native.Durability : Object.freeze({ Fast: 0, Flush: 1 });
const ErrorKind = native ? native.ErrorKind : ERROR_KIND_VALUES;

function makeNativeUnavailableError() {
  const reason = nativeLoadError instanceof Error ? nativeLoadError.message : "unknown load error";
  return new Error(
    `plasmite native addon is unavailable for ${process.platform}-${process.arch} (${reason}). ` +
      `expected addon path: ${nativeAddonPath ?? "unsupported platform"}. ` +
      "RemoteClient remains supported without native artifacts.",
  );
}

class PlasmiteNativeError extends Error {
  constructor(message, details = {}, cause = undefined) {
    super(message);
    this.name = "PlasmiteNativeError";
    this.kind = details.kind;
    this.path = details.path;
    this.seq = details.seq;
    this.offset = details.offset;
    this.cause = cause;
  }
}

function parseNativeError(err) {
  if (!(err instanceof Error) || !Object.hasOwn(err, "kind")) {
    return null;
  }
  return new PlasmiteNativeError(err.message, {
    kind: mapErrorKind(err.kind, ErrorKind.Io),
    path: err.path,
    seq: err.seq,
    offset: err.offset,
  }, err);
}

function wrapNativeError(err) {
  return parseNativeError(err) ?? err;
}

function isNativeNotFoundError(err) {
  if (!(err instanceof PlasmiteNativeError)) {
    return false;
  }
  return err.kind === ErrorKind.NotFound;
}

class Client {
  /**
   * Create a local client bound to a pool directory.
   * @param {string} [poolDir]
   * @returns {Client}
   */
  constructor(poolDir = DEFAULT_POOL_DIR) {
    if (!native) {
      throw makeNativeUnavailableError();
    }
    this._inner = new native.Client(poolDir);
  }

  /**
   * Create a new pool.
   * @param {string} poolRef
   * @param {number|bigint} [sizeBytes]
   * @returns {Pool}
   */
  createPool(poolRef, sizeBytes = DEFAULT_POOL_SIZE_BYTES) {
    try {
      return new Pool(this._inner.createPool(poolRef, sizeBytes));
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Open an existing pool.
   * @param {string} poolRef
   * @returns {Pool}
   */
  openPool(poolRef) {
    try {
      return new Pool(this._inner.openPool(poolRef));
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Open a pool if it exists, otherwise create it.
   * @param {string} poolRef
   * @param {number|bigint} [sizeBytes]
   * @returns {Pool}
   */
  pool(poolRef, sizeBytes = DEFAULT_POOL_SIZE_BYTES) {
    try {
      return this.openPool(poolRef);
    } catch (err) {
      if (isNativeNotFoundError(err)) {
        return this.createPool(poolRef, sizeBytes);
      }
      throw err;
    }
  }

  /**
   * Close the client handle.
   * @returns {void}
   */
  close() {
    this._inner.close();
  }

  [Symbol.dispose]() {
    this.close();
  }
}

class Pool {
  /**
   * @param {object} inner
   * @returns {Pool}
   */
  constructor(inner) {
    if (!native) {
      throw makeNativeUnavailableError();
    }
    this._inner = inner;
  }

  /**
   * Append JSON bytes and return raw message bytes.
   * @param {unknown} payload
   * @param {string[]} [tags]
   * @param {number} [durability]
   * @returns {Buffer}
   */
  appendJson(payload, tags, durability) {
    const input = Buffer.isBuffer(payload)
      ? payload
      : Buffer.from(JSON.stringify(payload));
    try {
      return this._inner.appendJson(input, tags ?? [], durability ?? Durability.Fast);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Append payload and return parsed Message.
   * @param {unknown} payload
   * @param {string[]} [tags]
   * @param {number} [durability]
   * @returns {Message}
   */
  append(payload, tags, durability) {
    return parseMessage(this.appendJson(payload, tags, durability));
  }

  /**
   * Append a Lite3 frame payload.
   * @param {Buffer} payload
   * @param {number} [durability]
   * @returns {bigint}
   */
  appendLite3(payload, durability) {
    try {
      return this._inner.appendLite3(payload, durability ?? Durability.Fast);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Get raw message bytes by sequence.
   * @param {number|bigint} seq
   * @returns {Buffer}
   */
  getJson(seq) {
    try {
      return this._inner.getJson(seq);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Get parsed Message by sequence.
   * @param {number|bigint} seq
   * @returns {Message}
   */
  get(seq) {
    return parseMessage(this.getJson(seq));
  }

  /**
   * Get Lite3 frame by sequence.
   * @param {number|bigint} seq
   * @returns {import("./types").Lite3Frame}
   */
  getLite3(seq) {
    try {
      return decorateLite3Frame(this._inner.getLite3(seq));
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Open a raw JSON stream.
   * @param {number|bigint|null} sinceSeq
   * @param {number|bigint|null} maxMessages
   * @param {number|bigint|null} timeoutMs
   * @param {boolean} [errorOnGap]
   * @returns {Stream}
   */
  openStream(sinceSeq, maxMessages, timeoutMs, errorOnGap = false) {
    try {
      return new Stream(
        this._inner.openStream(sinceSeq, maxMessages, timeoutMs, errorOnGap),
      );
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Open a raw Lite3 stream.
   * @param {number|bigint|null} sinceSeq
   * @param {number|bigint|null} maxMessages
   * @param {number|bigint|null} timeoutMs
   * @param {boolean} [errorOnGap]
   * @returns {Lite3Stream}
   */
  openLite3Stream(sinceSeq, maxMessages, timeoutMs, errorOnGap = false) {
    try {
      return new Lite3Stream(
        this._inner.openLite3Stream(
          sinceSeq,
          maxMessages,
          timeoutMs,
          errorOnGap,
        ),
      );
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Tail parsed messages with optional filtering.
   * @param {{sinceSeq?: number|bigint, maxMessages?: number|bigint, timeoutMs?: number|bigint, tags?: string[], errorOnGap?: boolean}} [options]
   * @returns {AsyncGenerator<Message, void, unknown>}
   */
  async *tail(options = {}) {
    const { sinceSeq, maxMessages, timeoutMs, tags, errorOnGap = false } = options;
    const requiredTags = normalizeTagFilter(tags);
    const limit = normalizeOptionalCount(maxMessages, "maxMessages");
    const pollTimeoutMs = normalizePollingTimeout(timeoutMs);

    let delivered = 0;
    let cursor = sinceSeq ?? null;
    while (true) {
      if (limit !== null && delivered >= limit) {
        return;
      }

      const remaining = limit === null ? null : limit - delivered;
      const streamLimit = requiredTags.length && remaining !== null ? null : remaining;
      const stream = this.openStream(
        cursor,
        streamLimit,
        pollTimeoutMs,
        errorOnGap,
      );
      let sawRawMessage = false;
      try {
        for (const message of stream) {
          sawRawMessage = true;
          const parsed = parseMessage(message);
          cursor = nextSinceSeq(parsed, cursor);
          if (!messageHasTags(parsed, requiredTags)) {
            continue;
          }
          delivered += 1;
          yield parsed;
          if (limit !== null && delivered >= limit) {
            return;
          }
        }
      } finally {
        stream.close();
      }
      if (limit !== null && !sawRawMessage) {
        return;
      }
      await sleep(0);
    }
  }

  /**
   * Replay parsed messages with original timing.
   * @param {{speed?: number, sinceSeq?: number|bigint, maxMessages?: number|bigint, timeoutMs?: number|bigint, tags?: string[]}} [options]
   * @returns {AsyncGenerator<Message, void, unknown>}
   */
  async *replay(options = {}) {
    const { speed = 1.0, sinceSeq, maxMessages, timeoutMs, tags } = options;
    if (speed <= 0) {
      throw new Error("speed must be positive");
    }

    const requiredTags = normalizeTagFilter(tags);
    const limit = normalizeOptionalCount(maxMessages, "maxMessages");
    const pollTimeoutMs = normalizePollingTimeout(timeoutMs);
    const streamLimit = requiredTags.length && limit !== null ? null : limit;
    const stream = this.openStream(
      sinceSeq ?? null,
      streamLimit,
      pollTimeoutMs,
    );

    const messages = [];
    try {
      for (const message of stream) {
        const parsed = parseMessage(message);
        if (!messageHasTags(parsed, requiredTags)) {
          continue;
        }
        messages.push({
          message: parsed,
          timeMs: messageTimeMs(parsed),
        });
        if (limit !== null && messages.length >= limit) {
          break;
        }
      }
    } finally {
      stream.close();
    }

    let prevMs = null;
    for (const entry of messages) {
      if (prevMs !== null && entry.timeMs !== null && speed > 0) {
        const delay = (entry.timeMs - prevMs) / speed;
        if (delay > 0) {
          await sleep(delay);
        }
      }
      if (entry.timeMs !== null) {
        prevMs = entry.timeMs;
      }
      yield entry.message;
    }
  }

  /**
   * Close the pool handle.
   * @returns {void}
   */
  close() {
    this._inner.close();
  }

  [Symbol.dispose]() {
    this.close();
  }
}

function normalizeTagFilter(tags) {
  if (tags === undefined || tags === null) {
    return [];
  }
  return Array.isArray(tags) ? tags : [tags];
}

function messageHasTags(message, requiredTags) {
  if (!requiredTags.length) {
    return true;
  }
  const messageTags = message && message.meta && Array.isArray(message.meta.tags)
    ? message.meta.tags
    : null;
  if (!messageTags) {
    return false;
  }
  return requiredTags.every((tag) => messageTags.includes(tag));
}

function messageTimeMs(message) {
  if (!message || !(message.time instanceof Date)) {
    return null;
  }
  const value = message.time.getTime();
  return Number.isFinite(value) ? value : null;
}

function nextSinceSeq(message, fallback) {
  if (!message) {
    return fallback;
  }
  if (typeof message.seq === "bigint") {
    return message.seq + 1n;
  }
  if (typeof message.seq === "number" && Number.isFinite(message.seq)) {
    return message.seq + 1;
  }
  return fallback;
}

function decorateLite3Frame(frame) {
  if (!frame || typeof frame !== "object") {
    return frame;
  }
  if (!Object.prototype.hasOwnProperty.call(frame, "time")) {
    Object.defineProperty(frame, "time", {
      configurable: false,
      enumerable: true,
      get() {
        return new Date(Number(this.timestampNs) / 1_000_000);
      },
    });
  }
  return frame;
}

function normalizeOptionalCount(value, fieldName) {
  if (value === undefined || value === null) {
    return null;
  }
  if (typeof value === "bigint") {
    if (value < 0n) {
      throw new TypeError(`${fieldName} must be non-negative`);
    }
    return Number(value);
  }
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new TypeError(`${fieldName} must be non-negative`);
  }
  return Math.floor(value);
}

function normalizePollingTimeout(timeoutMs) {
  if (timeoutMs === undefined || timeoutMs === null) {
    return 1000;
  }
  if (typeof timeoutMs === "bigint") {
    if (timeoutMs <= 0n) {
      return 1000;
    }
    return Number(timeoutMs);
  }
  if (typeof timeoutMs !== "number" || !Number.isFinite(timeoutMs)) {
    throw new TypeError("timeoutMs must be numeric");
  }
  if (timeoutMs <= 0) {
    return 1000;
  }
  return timeoutMs;
}

class Stream {
  /**
   * @param {object} inner
   * @returns {Stream}
   */
  constructor(inner) {
    if (!native) {
      throw makeNativeUnavailableError();
    }
    this._inner = inner;
  }

  /**
   * Read the next raw JSON message.
   * @returns {Buffer|null}
   */
  nextJson() {
    try {
      return this._inner.nextJson();
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Iterate raw JSON messages.
   * @returns {Iterator<Buffer>}
   */
  *[Symbol.iterator]() {
    let message;
    while ((message = this.nextJson()) !== null) {
      yield message;
    }
  }

  /**
   * Close the stream handle.
   * @returns {void}
   */
  close() {
    this._inner.close();
  }

  [Symbol.dispose]() {
    this.close();
  }
}

class Lite3Stream {
  /**
   * @param {object} inner
   * @returns {Lite3Stream}
   */
  constructor(inner) {
    if (!native) {
      throw makeNativeUnavailableError();
    }
    this._inner = inner;
  }

  /**
   * Read the next Lite3 frame.
   * @returns {import("./types").Lite3Frame|null}
   */
  next() {
    try {
      return decorateLite3Frame(this._inner.next());
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Iterate Lite3 frames.
   * @returns {Iterator<import("./types").Lite3Frame>}
   */
  *[Symbol.iterator]() {
    let frame;
    while ((frame = this.next()) !== null) {
      yield frame;
    }
  }

  /**
   * Close the Lite3 stream handle.
   * @returns {void}
   */
  close() {
    this._inner.close();
  }

  [Symbol.dispose]() {
    this.close();
  }
}

const UdpStart = Object.freeze({
  new: () => Object.freeze({ kind: "new" }),
  last: (count) => Object.freeze({ kind: "last", count: normalizeUint64(count, "count") }),
  since: (timestampNs) => Object.freeze({ kind: "since", timestampNs: normalizeUint64(timestampNs, "timestampNs") }),
  sequence: (seq) => Object.freeze({ kind: "sequence", seq: normalizeUint64(seq, "seq") }),
});

class UdpClient {
  constructor(endpoint) {
    if (!native) {
      throw makeNativeUnavailableError();
    }
    if (typeof endpoint !== "string" || !endpoint) {
      throw new TypeError("endpoint is required");
    }
    try {
      this._inner = new native.UdpClient(endpoint);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  openPool(poolName) {
    try {
      return new UdpPool(this._inner.openPool(poolName));
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  close() {
    this._inner.close();
  }

  [Symbol.dispose]() {
    this.close();
  }
}

class UdpPool {
  constructor(inner) {
    this._inner = inner;
  }

  sendJson(payload, tags = []) {
    const encoded = JSON.stringify(payload);
    if (encoded === undefined) {
      throw new TypeError("payload must be JSON serializable");
    }
    try {
      this._inner.sendJson(Buffer.from(encoded), tags);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  sendLite3(payload) {
    if (!Buffer.isBuffer(payload)) {
      throw new TypeError("payload must be a Buffer containing a complete Lite3 document");
    }
    try {
      this._inner.sendLite3(payload);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  follow(options = {}) {
    const start = options.start ?? UdpStart.new();
    const live = options.live ?? true;
    let startKind;
    let startValue = 0n;
    switch (start.kind) {
      case "new":
        startKind = 0;
        break;
      case "last":
        startKind = 1;
        startValue = normalizeUint64(start.count, "start.count");
        break;
      case "since":
        startKind = 2;
        startValue = normalizeUint64(start.timestampNs, "start.timestampNs");
        break;
      case "sequence":
        startKind = 3;
        startValue = normalizeUint64(start.seq, "start.seq");
        break;
      default:
        throw new TypeError("start must come from UdpStart.new, last, since, or sequence");
    }
    try {
      return this._inner
        .follow(startKind, BigInt(startValue), Boolean(live))
        .then((inner) => new UdpFollow(inner), (err) => {
          throw wrapNativeError(err);
        });
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  close() {
    this._inner.close();
  }

  [Symbol.dispose]() {
    this.close();
  }
}

class UdpFollow {
  constructor(inner) {
    this._inner = inner;
    this._closed = false;
    this._cancelled = false;
  }

  async nextEvent(timeoutMs = 1000, { signal } = {}) {
    const timeout = normalizeUint64(timeoutMs, "timeoutMs");
    const deadline = process.hrtime.bigint() + timeout * 1_000_000n;
    for (;;) {
      if (signal?.aborted) {
        throw abortError(signal);
      }
      if (this._closed || this.finished) {
        return null;
      }

      const remainingNs = deadline - process.hrtime.bigint();
      if (remainingNs <= 0n && timeout !== 0n) {
        return null;
      }
      try {
        // Keep native polling nonblocking so another follower, timer, or
        // AbortSignal can run while this call waits for the next event.
        const wait = this._inner.nextEvent(0n);
        if (signal?.aborted) {
          throw abortError(signal);
        }
        if (wait.status === 1) {
          return decorateUdpEvent(wait.event);
        }
        if (wait.status === 2 || wait.status === 3 || timeout === 0n) {
          return null;
        }
      } catch (err) {
        if (err?.name === "AbortError") {
          throw err;
        }
        throw wrapNativeError(err);
      }
      if (process.hrtime.bigint() >= deadline) {
        return null;
      }
      const untilDeadlineMs = (deadline - process.hrtime.bigint() + 999_999n) / 1_000_000n;
      await sleep(Number(untilDeadlineMs < 10n ? untilDeadlineMs : 10n));
    }
  }

  get finished() {
    return this._closed || this._cancelled || this._inner.isFinished;
  }

  async *events({ pollIntervalMs = 50, signal } = {}) {
    const timeoutMs = normalizeUint64(pollIntervalMs, "pollIntervalMs");
    while (true) {
      const event = await this.nextEvent(timeoutMs, { signal });
      if (event) {
        yield event;
        continue;
      }
      if (this.finished) {
        return;
      }
      await new Promise((resolve) => setImmediate(resolve));
    }
  }

  [Symbol.asyncIterator]() {
    return this.events();
  }

  cancel() {
    if (!this._closed) {
      this._cancelled = true;
      this._inner.cancel();
    }
  }

  close() {
    if (!this._closed) {
      this._closed = true;
      this._inner.close();
    }
  }

  [Symbol.dispose]() {
    this.close();
  }
}

function abortError(signal) {
  const reason = signal?.reason;
  if (reason instanceof Error) {
    return reason;
  }
  return new DOMException(
    reason === undefined ? "The operation was aborted" : String(reason),
    "AbortError",
  );
}

function decorateUdpEvent(event) {
  if (event.kind === "frame") {
    return Object.freeze({
      kind: "frame",
      seq: event.seq,
      timestampNs: event.timestampNs,
      payload: event.payload,
      message: parseMessage(event.message),
    });
  }
  if (event.kind === "gap") {
    return Object.freeze({
      kind: "gap",
      first: event.first,
      last: event.last,
      reason: event.reason,
    });
  }
  return Object.freeze({
    kind: "progress",
    startSeq: event.startSeq,
    nextSeq: event.nextSeq,
    endSeq: event.endSeq ?? null,
    terminal: Boolean(event.terminal),
    retentionGap: Boolean(event.retentionGap),
  });
}

function normalizeUint64(value, name) {
  let integer;
  if (typeof value === "bigint") {
    integer = value;
  } else if (typeof value === "number" && Number.isSafeInteger(value)) {
    integer = BigInt(value);
  } else {
    throw new TypeError(`${name} must be a bigint or safe integer`);
  }
  if (integer < 0n || integer > UINT64_MAX) {
    throw new RangeError(`${name} must fit an unsigned 64-bit integer`);
  }
  return integer;
}

/**
 * Backward-compatible replay helper.
 * @param {Pool} pool
 * @param {object} [options]
 * @returns {AsyncGenerator<Message, void, unknown>}
 */
async function* replay(pool, options = {}) {
  yield* pool.replay(options);
}

module.exports = {
  Client,
  Pool,
  Stream,
  Lite3Stream,
  UdpClient,
  UdpPool,
  UdpFollow,
  UdpStart,
  Message,
  DEFAULT_POOL_DIR,
  DEFAULT_POOL_SIZE,
  DEFAULT_POOL_SIZE_BYTES,
  Durability,
  ErrorKind,
  PlasmiteNativeError,
  RemoteClient,
  RemoteError,
  RemotePool,
  parseMessage,
  replay,
};
