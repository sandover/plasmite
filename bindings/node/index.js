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
  if (!(err instanceof Error) || typeof err.message !== "string") {
    return null;
  }
  const prefix = "plasmite error:";
  if (!err.message.startsWith(prefix)) {
    return null;
  }
  const parts = err.message
    .slice(prefix.length)
    .split(";")
    .map((part) => part.trim())
    .filter(Boolean);
  if (parts.length === 0) {
    return null;
  }
  const details = {};
  for (const part of parts) {
    const [key, ...valueParts] = part.split("=");
    if (!key || valueParts.length === 0) {
      continue;
    }
    const value = valueParts.join("=");
    if (key === "seq" || key === "offset") {
      const parsed = Number(value);
      details[key] = Number.isFinite(parsed) ? parsed : undefined;
      continue;
    }
    if (key === "kind") {
      details.kind = mapErrorKind(value);
      continue;
    }
    details[key] = value;
  }
  if (details.kind === undefined) {
    details.kind = ErrorKind.Io;
  }
  return new PlasmiteNativeError(err.message, details, err);
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
   * @returns {Stream}
   */
  openStream(sinceSeq, maxMessages, timeoutMs) {
    try {
      return new Stream(this._inner.openStream(sinceSeq, maxMessages, timeoutMs));
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Open a raw Lite3 stream.
   * @param {number|bigint|null} sinceSeq
   * @param {number|bigint|null} maxMessages
   * @param {number|bigint|null} timeoutMs
   * @returns {Lite3Stream}
   */
  openLite3Stream(sinceSeq, maxMessages, timeoutMs) {
    try {
      return new Lite3Stream(
        this._inner.openLite3Stream(sinceSeq, maxMessages, timeoutMs),
      );
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Tail parsed messages with optional filtering.
   * @param {{sinceSeq?: number|bigint, maxMessages?: number|bigint, timeoutMs?: number|bigint, tags?: string[]}} [options]
   * @returns {AsyncGenerator<Message, void, unknown>}
   */
  async *tail(options = {}) {
    const { sinceSeq, maxMessages, timeoutMs, tags } = options;
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
      const stream = this.openStream(cursor, streamLimit, pollTimeoutMs);
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
