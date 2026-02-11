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

const PLATFORM_DIRS = Object.freeze({
  linux: Object.freeze({ x64: "linux-x64", arm64: "linux-arm64" }),
  darwin: Object.freeze({ x64: "darwin-x64", arm64: "darwin-arm64" }),
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
  constructor(poolDir) {
    if (!native) {
      throw makeNativeUnavailableError();
    }
    this._inner = new native.Client(poolDir);
  }

  createPool(poolRef, sizeBytes) {
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
    try {
      return this._inner.appendJson(payload, tags, durability);
    } catch (err) {
      throw wrapNativeError(err);
    }
  }

  appendLite3(payload, durability) {
    try {
      return this._inner.appendLite3(payload, durability);
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

  close() {
    this._inner.close();
  }
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

  close() {
    this._inner.close();
  }
}

async function* replay(pool, options = {}) {
  const { speed = 1.0, sinceSeq, maxMessages, timeoutMs } = options;
  const stream = pool.openStream(
    sinceSeq ?? null,
    maxMessages ?? null,
    timeoutMs ?? null,
  );

  const messages = [];
  try {
    let msg;
    while ((msg = stream.nextJson()) !== null) {
      messages.push(msg);
    }
  } finally {
    stream.close();
  }

  let prevMs = null;
  for (const msg of messages) {
    const parsed = JSON.parse(msg);
    const curMs = new Date(parsed.time).getTime();

    if (prevMs !== null && speed > 0) {
      const delay = (curMs - prevMs) / speed;
      if (delay > 0) {
        await sleep(delay);
      }
    }

    prevMs = curMs;
    yield msg;
  }
}

module.exports = {
  Client,
  Pool,
  Stream,
  Lite3Stream,
  Durability: native ? native.Durability : Object.freeze({}),
  ErrorKind: native ? native.ErrorKind : Object.freeze({}),
  PlasmiteNativeError,
  RemoteClient,
  RemoteError,
  RemotePool,
  RemoteTail,
  replay,
};
