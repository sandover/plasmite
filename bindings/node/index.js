/*
Purpose: JavaScript entry point for the Plasmite Node binding.
Key Exports: Client, Pool, Stream, Durability, ErrorKind, replay.
Role: Thin wrapper around the native N-API addon.
Invariants: Exports align with native symbols and v0 API semantics.
Notes: Requires libplasmite to be discoverable at runtime.
*/

const { RemoteClient, RemoteError, RemotePool, RemoteTail } = require("./remote");

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
const DEFAULT_POOL_SIZE = 4 * 1024 * 1024;
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

const Durability = native ? native.Durability : Object.freeze({});
const ErrorKind = native ? native.ErrorKind : Object.freeze({});

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
    details[key] = value;
  }
  return new PlasmiteNativeError(err.message, details, err);
}

function wrapNativeError(err) {
  return parseNativeError(err) ?? err;
}

class Client {
  constructor(poolDir = DEFAULT_POOL_DIR) {
    if (!native) {
      throw makeNativeUnavailableError();
    }
    this._inner = new native.Client(poolDir);
  }

  createPool(poolRef, sizeBytes = DEFAULT_POOL_SIZE_BYTES) {
    try {
      return new Pool(this._inner.createPool(poolRef, sizeBytes));
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  openPool(poolRef) {
    try {
      return new Pool(this._inner.openPool(poolRef));
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  close() {
    this._inner.close();
  }
}

class Pool {
  constructor(inner) {
    if (!native) {
      throw makeNativeUnavailableError();
    }
    this._inner = inner;
  }

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

  append(payload, tags, durability) {
    return this.appendJson(payload, tags, durability);
  }

  appendLite3(payload, durability) {
    try {
      return this._inner.appendLite3(payload, durability ?? Durability.Fast);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  getJson(seq) {
    try {
      return this._inner.getJson(seq);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  get(seq) {
    return this.getJson(seq);
  }

  getLite3(seq) {
    try {
      return this._inner.getLite3(seq);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  openStream(sinceSeq, maxMessages, timeoutMs) {
    try {
      return new Stream(this._inner.openStream(sinceSeq, maxMessages, timeoutMs));
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  openLite3Stream(sinceSeq, maxMessages, timeoutMs) {
    try {
      return new Lite3Stream(
        this._inner.openLite3Stream(sinceSeq, maxMessages, timeoutMs),
      );
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

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
          yield message;
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
          message,
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

  close() {
    this._inner.close();
  }
}

function parseMessage(buf) {
  return JSON.parse(buf.toString("utf8"));
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
  if (!message || typeof message.time !== "string") {
    return null;
  }
  const value = new Date(message.time).getTime();
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
  constructor(inner) {
    if (!native) {
      throw makeNativeUnavailableError();
    }
    this._inner = inner;
  }

  nextJson() {
    try {
      return this._inner.nextJson();
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  *[Symbol.iterator]() {
    let message;
    while ((message = this.nextJson()) !== null) {
      yield message;
    }
  }

  close() {
    this._inner.close();
  }
}

class Lite3Stream {
  constructor(inner) {
    if (!native) {
      throw makeNativeUnavailableError();
    }
    this._inner = inner;
  }

  next() {
    try {
      return this._inner.next();
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  *[Symbol.iterator]() {
    let frame;
    while ((frame = this.next()) !== null) {
      yield frame;
    }
  }

  close() {
    this._inner.close();
  }
}

async function* replay(pool, options = {}) {
  yield* pool.replay(options);
}

module.exports = {
  Client,
  Pool,
  Stream,
  Lite3Stream,
  DEFAULT_POOL_DIR,
  DEFAULT_POOL_SIZE,
  DEFAULT_POOL_SIZE_BYTES,
  Durability,
  ErrorKind,
  PlasmiteNativeError,
  RemoteClient,
  RemoteError,
  RemotePool,
  RemoteTail,
  parseMessage,
  replay,
};
