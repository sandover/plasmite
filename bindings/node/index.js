/*
Purpose: JavaScript entry point for the Plasmite Node binding.
Key Exports: Client, Pool, Message, Stream, Durability, ErrorKind.
Role: Thin wrapper around the native N-API addon.
Invariants: Exports align with native symbols and v0 API semantics.
Notes: Requires libplasmite to be discoverable at runtime.
*/

const { RemoteClient, RemoteError, RemotePool } = require("./remote");
const { Message, parseMessage, parseNativeMessage } = require("./message");
const { ERROR_KIND_VALUES, mapErrorKind } = require("./mappings");

const { createNativeMessage: ownedMessage, toNative, toJson, toLite3, messageReference, valueReference } = require("./views");
const { inputHelpers, snapshotTags, configureNativeInput, nativeOwner } = require("./input");

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

function defaultPoolDir() {
  let home;
  try {
    if (process.platform !== "win32" && process.env.HOME && path.isAbsolute(process.env.HOME)) {
      home = process.env.HOME;
    } else if (process.platform === "win32") {
      home = process.env.USERPROFILE;
      if (!home || !path.isAbsolute(home)) home = os.userInfo().homedir;
    } else {
      home = os.userInfo().homedir;
    }
  } catch (cause) {
    throw new Error("cannot determine the user home directory; supply an explicit pool directory", { cause });
  }
  if (!home || !path.isAbsolute(home)) {
    throw new Error("cannot determine the user home directory; supply an explicit pool directory");
  }
  return path.join(home, ".plasmite", "pools");
}

// An unavailable default must not prevent remote-only or explicit-directory use.
let DEFAULT_POOL_DIR;
try { DEFAULT_POOL_DIR = defaultPoolDir(); } catch {}
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
configureNativeInput(native?.nativeViewFromBytes);

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
  constructor(poolDir = defaultPoolDir()) {
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
    return parseNativeMessage(this.appendJson(payload, tags, durability));
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

  /** Return the committed sequence and Unix timestamp in nanoseconds. */
  appendIdentity(payload, tags, durability) {
    const input = Buffer.isBuffer(payload)
      ? payload
      : Buffer.from(JSON.stringify(payload));
    try {
      return this._inner.appendJsonIdentity(input, tags ?? [], durability ?? Durability.Fast);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /** Append binary data and return its committed numeric identity. */
  appendLite3Identity(payload, durability) {
    try {
      return this._inner.appendLite3Identity(payload, durability ?? Durability.Fast);
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

  /** Encode native input once and return its immutable committed view. */
  async appendView(data, tags, durability) {
    let target;
    try { target = this._inner.prepareAppend(durability ?? Durability.Fast); }
    catch (error) { throw wrapNativeError(error); }
    // Keep this target separate from Pool before invoking any input hooks.
    let pending;
    try {
      const tagSnapshot = snapshotTags(tags);
      const message = messageReference(data);
      if (message && tags === undefined) pending = target.forward(nativeOwner(message.backend));
      else {
        const payload = message ? data.data : data;
        const ref = valueReference(payload);
        pending = ref
          ? target.copy(nativeOwner(ref.backend), ref.node, tagSnapshot)
          : target.encode(payload, tagSnapshot, inputHelpers);
      }
    } catch (error) { target.discard(); throw error; }
    let owner;
    try { owner = await pending; }
    catch (error) { throw wrapNativeError(error); }
    return ownedMessage(owner);
  }

  /** Acquire an immutable snapshot by sequence. */
  async getView(seq) {
    try {
      const owner = await this._inner.getView(normalizeUint64(seq, "seq"));
      return ownedMessage(owner);
    } catch (err) { throw wrapNativeError(err); }
  }

  get(seq) {
    return parseNativeMessage(this.getJson(seq));
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
   * @param {{sinceSeq?: number|bigint, maxMessages?: number|bigint, timeoutMs?: number|bigint, tags?: string[], errorOnGap?: boolean, signal?: AbortSignal}} [options]
   * @returns {AsyncGenerator<Message, void, unknown>}
   */
  async *tail(options = {}) {
    const { sinceSeq, maxMessages, timeoutMs, tags, errorOnGap = false, signal } = options;
    const requiredTags = normalizeTagFilter(tags);
    const limit = normalizeOptionalCount(maxMessages, "maxMessages");
    const waitMs = BigInt(Math.floor(normalizePollingTimeout(timeoutMs)));
    if (limit === 0 || signal?.aborted) return;
    const stream = this.openStream(sinceSeq ?? null, requiredTags.length ? null : limit, null, errorOnGap);
    const abort = () => stream.cancel();
    signal?.addEventListener("abort", abort, { once: true });
    let delivered = 0;
    try {
      while (!signal?.aborted) {
        const result = await stream.nextJsonWait(waitMs);
        if (signal?.aborted || result.status === 2 || result.status === 3) return;
        if (result.status === 0) {
          if (limit !== null) return;
          continue;
        }
        const message = parseNativeMessage(result.message);
        if (messageHasTags(message, requiredTags)) {
          delivered++;
          yield message;
          if (limit !== null && delivered >= limit) return;
        }
      }
    } finally {
      signal?.removeEventListener("abort", abort);
      stream.close();
    }
  }

  /** Pull independent immutable snapshots; no next item starts until demand. */
  tailViews(options = {}) {
    const pool = this;
    let stream;
    let stopped = false;
    async function* read() {
      const { sinceSeq, maxMessages, timeoutMs, errorOnGap = false, signal } = options;
      const limit = normalizeOptionalCount(maxMessages, "maxMessages");
      const finiteWait = BigInt(Math.floor(normalizePollingTimeout(timeoutMs)));
      // An unlimited tail keeps one native read pending across quiet periods.
      const waitMs = limit === null ? 18446744073709551615n : finiteWait;
      if (signal?.aborted || stopped || limit === 0) return;
      const tags = snapshotTags(options.tags);
      try { stream = pool._inner.openViewStream(sinceSeq ?? null, maxMessages ?? null, null, errorOnGap, tags); }
      catch (error) { throw wrapNativeError(error); }
      const abort = () => stream.cancel();
      signal?.addEventListener("abort", abort, { once: true });
      try {
        while (!signal?.aborted && !stopped) {
          let result;
          try { result = await stream.nextJsonWait(waitMs); }
          catch (error) { throw wrapNativeError(error); }
          if (stopped || signal?.aborted || result.status === 2 || result.status === 3) return;
          if (result.status === 0) { if (limit !== null) return; continue; }
          if (result.status === 1) yield ownedMessage(result.view);
        }
      } finally { signal?.removeEventListener("abort", abort); stream.close(); }
    }
    return cancelOnReturn(read(), () => { stopped = true; stream?.cancel(); });
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

  /** Read on a native worker: 0=wait expired, 1=value, 2=finished, 3=cancelled. */
  async nextJsonWait(timeoutMs) {
    try {
      const result = await this._inner.nextJsonWait(normalizeUint64(timeoutMs, "timeoutMs"));
      return result;
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  cancel() {
    this._inner.cancel();
  }

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

  /** Read on a native worker: 0=wait expired, 1=value, 2=finished, 3=cancelled. */
  async nextWait(timeoutMs) {
    try {
      const result = await this._inner.nextWait(normalizeUint64(timeoutMs, "timeoutMs"));
      if (result.frame) decorateLite3Frame(result.frame);
      return result;
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  cancel() {
    this._inner.cancel();
  }

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

  follow(options = {}) { return this._follow(options, false); }

  followViews(options = {}) { return this._follow(options, true); }

  _follow(options, views) {
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
        .follow(startKind, BigInt(startValue), Boolean(live), views)
        .then((inner) => new UdpFollow(inner, views), (err) => {
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
  constructor(inner, views = false) {
    this._inner = inner;
    this._views = views;
    this._closed = false;
    this._cancelled = false;
  }

  async nextEvent(timeoutMs = 1000, { signal } = {}) {
    const timeout = normalizeUint64(timeoutMs, "timeoutMs");
    if (signal?.aborted) throw abortError(signal);
    if (this._closed || this.finished) return null;
    const abort = () => this._inner.interruptRead();
    signal?.addEventListener("abort", abort, { once: true });
    try {
      const result = await this._inner.nextEventWait(timeout);
      if (signal?.aborted) throw abortError(signal);
      if (result.status !== 1) return null;
      if (this._views && result.view) {
        const message = ownedMessage(result.view);
        return { kind: "frame", seq: message.seq, timestampNs: message.timestampNs, message };
      }
      return decorateUdpEvent(result.event);
    } catch (err) {
      if (err?.name === "AbortError") throw err;
      throw wrapNativeError(err);
    } finally {
      signal?.removeEventListener("abort", abort);
    }
  }

  get finished() {
    return this._closed || this._cancelled || this._inner.isFinished;
  }

  events({ pollIntervalMs = 50, signal } = {}) {
    const follow = this;
    const timeoutMs = normalizeUint64(pollIntervalMs, "pollIntervalMs");
    let stopped = false;
    let reading = false;
    async function* read() {
      while (!stopped) {
        let event;
        reading = true;
        try { event = await follow.nextEvent(timeoutMs, { signal }); }
        finally { reading = false; }
        if (stopped) return;
        if (event) { yield event; continue; }
        if (follow.finished) return;
        await new Promise((resolve) => setImmediate(resolve));
      }
    }
    return cancelOnReturn(read(), () => { stopped = true; if (reading) follow._inner.interruptRead(); });
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

// Async generators queue return/throw behind a pending next(). Interrupt the
// native read first so explicit iterator cleanup also works on an idle stream.
function cancelOnReturn(iterator, cancel) {
  const end = iterator.return.bind(iterator), fail = iterator.throw.bind(iterator);
  iterator.return = value => { cancel(); return end(value); };
  iterator.throw = error => { cancel(); return fail(error); };
  return iterator;
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

module.exports = {
  toNative, toJson, toLite3,
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
};
