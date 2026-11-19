const { at, readStart, normalizeWait } = require("./follow");
/*
Purpose: JavaScript entry point for the Plasmite Node binding.
Key Exports: Client, Pool, Message, Stream, Durability, ErrorKind.
Role: Thin wrapper around the native N-API addon.
Invariants: Exports align with native symbols and 2.0 API semantics.
Notes: Requires libplasmite to be discoverable at runtime.
*/

const { RemoteClient, RemoteError, RemotePool } = require("./remote");
const { Message, parseMessage, parseNativeMessage, parsePoolInfo } = require("./message");
const { ERROR_KIND_VALUES, mapErrorKind } = require("./mappings");

const { createNativeMessage: ownedMessage, toNative, toJson, toLite3, messageReference, valueReference } = require("./views");
const { snapshotTags, configureNativeInput, nativeOwner, captureNativeInput } = require("./input");

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
const DEFAULT_POOL_SIZE_BYTES = 1024 * 1024;
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
configureNativeInput(native?.nativeViewFromBytes, native?.NativeView);

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
    this.missing = details.missing;
    this.nextSeq = details.nextSeq;
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
    missing: err.missing,
    nextSeq: err.nextSeq,
  }, err);
}

function wrapNativeError(err) {
  return parseNativeError(err) ?? err;
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
    let size;
    try { size = normalizeUint64(sizeBytes, "sizeBytes"); }
    catch (err) { throw usageError(err.message); }
    try {
      return new Pool(this._inner.createPool(poolRef, size));
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Open an existing pool.
   * @param {string} poolRef
   * @returns {Pool}
   */
  openPool(poolRef, { create = false } = {}) {
    try {
      return new Pool(this._inner.openPool(poolRef, create));
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  listPools() {
    try {
      return parsePoolInfo(this._inner.listPoolsJson());
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  deletePool(poolRef) {
    try {
      this._inner.deletePool(poolRef);
    } catch (err) {
      throw wrapNativeError(err);
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

  async feedLite3(payload, { tags, durability = Durability.Fast } = {}) {
    if (tags !== undefined) throw usageError("cannot supply tags when forwarding a message");
    try {
      return parseNativeMessage(await this._inner.feedLite3(payload, durability));
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  async feedReceipt(value, { tags, durability = Durability.Fast } = {}) {
    const message = messageReference(value);
    if (message) {
      if (tags !== undefined) throw usageError("cannot supply tags when forwarding a message");
      return this.feedLite3Receipt(toLite3(value), { durability });
    }
    const [data, labels] = feedInput(value, tags);
    return this.feedJsonReceipt(Buffer.from(JSON.stringify(data)), { tags: labels, durability });
  }

  async feedJsonReceipt(payload, { tags = [], durability = Durability.Fast } = {}) {
    try {
      return await this._inner.feedJsonReceipt(payload, tags, durability);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  async feedLite3Receipt(payload, { tags, durability = Durability.Fast } = {}) {
    if (tags !== undefined) throw usageError("cannot supply tags when forwarding a message");
    try {
      return await this._inner.feedLite3Receipt(payload, durability);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /**
   * Get raw message bytes by sequence.
   * @param {number|bigint} seq
   * @returns {Buffer}
   */
  _getJson(seq) {
    try {
      const stream = this._inner.openRead(true, 2, normalizeUint64(seq, "seq"), [], false);
      try { return stream.nextJson() ?? missingMessage(seq); } finally { stream.close(); }
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  /** Encode native input once and return its immutable committed view. */
  async feedView(data, { tags, durability = Durability.Fast } = {}) {
    if (data instanceof Message) {
      if (tags !== undefined) throw usageError("cannot supply tags when forwarding a message");
      tags = data.tags;
      data = data.data;
    }
    const message = messageReference(data);
    if (message && tags !== undefined) {
      throw usageError("cannot supply tags when forwarding a message");
    }
    let target;
    try { target = this._inner.prepareAppend(durability); }
    catch (error) { throw wrapNativeError(error); }
    // Keep the target alive when input hooks close the Pool.
    let pending;
    try {
      if (message) pending = target.forward(nativeOwner(message.backend));
      else {
        const labels = snapshotTags(tags);
        const ref = valueReference(data);
        if (ref) pending = target.copy(nativeOwner(ref.backend), ref.node, labels);
        else pending = captureNativeInput(target, data, labels);
      }
    } catch (error) { target.discard(); throw error; }
    try { return ownedMessage(await pending); }
    catch (error) { throw wrapNativeError(error); }
  }

  /** Acquire an immutable snapshot by sequence. */
  async _getView(seq) {
    try {
      const owner = await this._inner.getView(normalizeUint64(seq, "seq"));
      return ownedMessage(owner);
    } catch (err) { throw wrapNativeError(err); }
  }

  /**
   * Get Lite3 frame by sequence.
   * @param {number|bigint} seq
   * @returns {import("./types").Lite3Frame}
   */
  _getLite3(seq) {
    try {
      const stream = this._inner.openLite3Read(true, 2, normalizeUint64(seq, "seq"), []);
      try { return stream.next() ?? missingMessage(seq); } finally { stream.close(); }
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  follow(options = {}) {
    return this._read(false, options);
  }

  fetch(options = {}) {
    return this._read(true, options);
  }

  followView(options = {}) {
    return this._read(false, options, "view");
  }

  fetchView(options = {}) {
    return this._read(true, options, "view");
  }

  followLite3(options = {}) { return this._read(false, options, "lite3"); }

  fetchLite3(options = {}) { return this._read(true, options, "lite3"); }

  async _read(finite, options = {}, representation) {
    const { since, last, tags, signal, ...extra } = options;
    if (Object.keys(extra).length) throw usageError("read options are since, last, tags and signal");
    let start;
    try { start = readStart(since, last); } catch (err) { throw usageError(err.message); }
    const [kind, value] = start;
    if (signal?.aborted) throw abortError(signal);
    try {
      const labels = normalizeTagFilter(tags);
      if (representation === "lite3") {
        const stream = new Lite3Stream(this._inner.openLite3Read(finite, kind, value, labels));
        return new Reader(stream, signal, result => result.frame);
      }
      const view = representation === "view";
      return new Reader(new Stream(this._inner.openRead(finite, kind, value,
        labels, view)), signal, view ? result => ownedMessage(result.view) : undefined);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  info() {
    try {
      return parsePoolInfo(this._inner.infoJson());
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  async feed(value, { tags, durability = Durability.Fast } = {}) {
    const message = messageReference(value);
    if (message) {
      if (tags !== undefined) throw usageError("cannot supply tags when forwarding a message");
      return this.feedLite3(toLite3(value), { durability });
    }
    const [data, labels] = feedInput(value, tags);
    return parseNativeMessage(await this.feedJson(Buffer.from(JSON.stringify(data)), { tags: labels, durability }));
  }

  async feedJson(payload, { tags = [], durability = Durability.Fast } = {}) {
    try {
      return await this._inner.feedJson(payload, tags, durability);
    } catch (err) {
      throw wrapNativeError(err);
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

function missingMessage(seq) {
  throw new PlasmiteNativeError("message not found", { kind: ErrorKind.NotFound, seq: BigInt(seq) });
}

function feedInput(value, tags) {
  if (value instanceof Message) {
    if (tags !== undefined) throw usageError("cannot supply tags when forwarding a message");
    tags = value.tags;
    value = value.data;
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw usageError("feed requires an object");
  }
  return [value, tags ?? []];
}

function usageError(message) {
  return new PlasmiteNativeError(message, { kind: ErrorKind.Usage });
}

class Reader {
  constructor(stream, signal, decode = result => parseNativeMessage(result.message)) {
    this._stream = stream;
    this._decode = decode;
    this._signal = signal;
    this._reading = false;
    this._poll = stream instanceof Lite3Stream ? timeout => stream.nextWait(timeout)
      : timeout => stream.nextJsonWait(timeout);
    this._abort = () => this.close();
    if (signal?.aborted) this.close();
    else signal?.addEventListener("abort", this._abort, { once: true });
  }

  async next(wait) {
    const stream = this._stream;
    if (!stream) return { done: true, value: undefined };
    let waitMs;
    if (wait !== undefined) {
      try { waitMs = normalizeWait(wait); } catch (err) { throw usageError(err.message); }
    }
    if (this._reading) {
      throw new PlasmiteNativeError("stream already has an active reader", { kind: ErrorKind.Busy });
    }
    this._reading = true;
    try {
      if (wait !== undefined) {
        const result = await this._poll(BigInt(waitMs));
        if (result.status === 0 && this._stream) return { done: false, value: undefined };
        if (result.status === 1 && this._stream) {
          return { done: false, value: this._decode(result) };
        }
      } else {
        while (this._stream) {
          const result = await this._poll(18446744073709551615n);
          if (!this._stream || result.status === 2 || result.status === 3) break;
          if (result.status === 1) return { done: false, value: this._decode(result) };
        }
      }
    } catch (err) {
      this.close();
      throw err;
    } finally {
      this._reading = false;
    }
    return this.return();
  }

  close() {
    this._signal?.removeEventListener("abort", this._abort);
    const stream = this._stream;
    this._stream = null;
    stream?.close();
  }

  async return() {
    this.close();
    return { done: true, value: undefined };
  }

  [Symbol.asyncIterator]() {
    return this;
  }
}

function normalizeTagFilter(tags) {
  if (tags === undefined || tags === null) {
    return [];
  }
  return Array.isArray(tags) ? tags : [tags];
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
      return this._inner.next();
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

  async feedLite3(payload) {
    if (!Buffer.isBuffer(payload)) {
      throw new TypeError("payload must be a Buffer containing a complete Lite3 document");
    }
    try {
      this._inner.feedLite3(payload);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  async feed(value, { tags, durability = Durability.Fast } = {}) {
    if (messageReference(value)) {
      if (tags !== undefined) throw usageError("cannot supply tags when forwarding a message");
      if (durability !== Durability.Fast) throw usageError("UDP feed only supports fast durability");
      return this.feedLite3(toLite3(value));
    }
    const [data, labels] = feedInput(value, tags);
    try {
      this._inner.feedJson(Buffer.from(JSON.stringify(data)), labels, durability);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  follow(options = {}) { return this._read(options, true, false); }

  fetch(options = {}) { return this._read(options, false, false); }

  followView(options = {}) { return this._read(options, true, true); }

  fetchView(options = {}) { return this._read(options, false, true); }

  async _read(options, live, view) {
    const { since, last, tags, signal, ...extra } = options;
    if (Object.keys(extra).length) throw usageError("read options are since, last, tags and signal");
    let start;
    try { start = readStart(since, last); } catch (err) { throw usageError(err.message); }
    const [kind, value] = start;
    if (signal?.aborted) throw abortError(signal);
    try {
      const reader = new UdpFollow(await this._inner.follow(kind, value, live, view,
        normalizeTagFilter(tags)), view, signal);
      if (signal?.aborted) {
        reader.close();
        throw abortError(signal);
      }
      return reader;
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
  constructor(inner, views = false, signal) {
    this._inner = inner;
    this._views = views;
    this._signal = signal;
    this._closed = false;
    this._abort = () => this.close();
    if (signal?.aborted) this.close();
    else signal?.addEventListener("abort", this._abort, { once: true });
  }

  async nextEvent(timeoutMs, options) {
    if (timeoutMs === undefined) return this._nextEvent(UINT64_MAX, options);
    if (typeof timeoutMs !== "number" || timeoutMs < 0 || !Number.isSafeInteger(Math.ceil(timeoutMs))) {
      throw usageError("timeout must be a non-negative duration in milliseconds");
    }
    return this._nextEvent(BigInt(Math.ceil(timeoutMs)), options);
  }

  async _nextEvent(timeout, { signal = this._signal } = {}) {
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
        return { kind: "frame", seq: message.seq, time: message.time, message };
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
    return this._closed || this._inner.isFinished;
  }

  async next(wait) {
    let timeout;
    try { timeout = wait === undefined ? UINT64_MAX : BigInt(normalizeWait(wait)); }
    catch (err) { throw usageError(err.message); }
    const value = await this._nextEvent(timeout);
    return { done: value === null && this.finished, value: value ?? undefined };
  }

  events({ signal = this._signal } = {}) {
    const follow = this;
    let started = false;
    const iteratorSignal = signal === this._signal ? undefined : signal;
    const close = () => follow.close();
    async function* read() {
      started = true;
      iteratorSignal?.addEventListener("abort", close, { once: true });
      try {
        while (true) {
          const event = await follow.nextEvent(undefined, { signal });
          if (follow._closed) return;
          if (event) { yield event; continue; }
          if (follow.finished) return;
          await new Promise((resolve) => setImmediate(resolve));
        }
      } finally {
        iteratorSignal?.removeEventListener("abort", close);
        close();
      }
    }
    return cancelOnReturn(read(), () => { if (started) close(); });
  }

  [Symbol.asyncIterator]() {
    return this.events();
  }

  cancel() {
    this.close();
  }

  close() {
    if (!this._closed) {
      this._closed = true;
      this._signal?.removeEventListener("abort", this._abort);
      this._inner.close();
    }
  }

  [Symbol.dispose]() {
    this.close();
  }
}

// Async generators queue return/throw behind a pending next(). Close the
// native read first so explicit iterator cleanup works on an idle stream.
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
      time: event.time,
      payload: event.payload,
      message: parseMessage(event.message),
    });
  }
  if (event.kind === "skip") {
    return Object.freeze({
      kind: "skip",
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

function withClient(action) {
  const client = new Client();
  try {
    return action(client);
  } finally {
    client.close();
  }
}

const createPool = (ref, size) => withClient(c => c.createPool(ref, size));
const openPool = (ref, opts) => withClient(c => c.openPool(ref, opts));
const listPools = () => withClient(c => c.listPools());
const deletePool = ref => withClient(c => c.deletePool(ref));

module.exports = {
  at, createPool, openPool, listPools, deletePool,
  toNative, toJson, toLite3,
  Client,
  Pool,
  Stream,
  Lite3Stream,
  UdpClient,
  UdpPool,
  UdpFollow,
  Message,
  DEFAULT_POOL_DIR,
  DEFAULT_POOL_SIZE_BYTES,
  Durability,
  ErrorKind,
  PlasmiteNativeError,
  RemoteClient,
  RemoteError,
  RemotePool,
  parseMessage,
};
