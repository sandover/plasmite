const { timeText } = require("./message");
// Immutable language views. Backends own checked Lite3 bytes and return node
// descriptors for containers/bytes; scalars cross directly and undefined means absent.
const refs = new WeakMap();
const messages = new WeakMap();
const readonly = () => { throw new TypeError("Plasmite views are read-only"); };
const indexOf = key => typeof key === "string" && /^(0|[1-9][0-9]*)$/.test(key)
  && Number(key) < 0xffffffff ? Number(key) : -1;
const validKey = key => typeof key === "string" && !key.includes("\0") && key.isWellFormed();

class Message {
  constructor() { throw new TypeError("Messages come from a Plasmite pool"); }
  get tags() { return this.meta.tags; }
  get sender() { return this.meta.sender; }
  get raw() { return toLite3(this); }
}

function createMessageView(backend, seq, timestampNs) {
  let cache;
  function wrap(value) {
    if (value === null || typeof value !== "object") return value;
    cache ??= new Map();
    const prior = cache.get(value.node)?.deref();
    if (prior) return prior;
    const array = value.kind === "array";
    const bytes = value.kind === "bytes";
    const target = array ? [] : Object.create(null);
    if (array || bytes) Object.defineProperty(target, "length", { value: value.count, writable: false });
    const lookup = key => {
      if (!validKey(key)) return undefined;
      if (array || bytes) {
        const index = indexOf(key);
        return index >= 0 && index < value.count ? backend.at(value.node, index) : undefined;
      }
      return backend.lookup(value.node, key);
    };
    const iterator = function* () {
      for (const item of backend.iterate(value.node)) yield wrap(item);
    };
    const proxy = new Proxy(target, {
      get(target, key, receiver) {
        if ((array || bytes) && key === "length") return value.count;
        if ((array || bytes) && key === Symbol.iterator) return iterator;
        const item = lookup(key);
        if (item !== undefined) return wrap(item);
        return array ? Reflect.get(target, key, receiver) : undefined;
      },
      has(target, key) {
        if (array || bytes) {
          const index = indexOf(key);
          return (index >= 0 && index < value.count) || Reflect.has(target, key);
        }
        return (validKey(key) && backend.has(value.node, key)) || Reflect.has(target, key);
      },
      ownKeys() {
        if (array || bytes) return [...Array.from({ length: value.count }, (_, i) => String(i)), "length"];
        const keys = backend.keys(value.node);
        return [...keys.filter(k => indexOf(k) >= 0).sort((a, b) => Number(a) - Number(b)),
          ...keys.filter(k => indexOf(k) < 0)];
      },
      getOwnPropertyDescriptor(target, key) {
        const actual = Reflect.getOwnPropertyDescriptor(target, key);
        if (actual) return actual;
        const item = lookup(key);
        return item === undefined ? undefined : { value: wrap(item), enumerable: true, writable: false, configurable: true };
      },
      set: readonly, defineProperty: readonly, deleteProperty: readonly,
      setPrototypeOf: readonly, preventExtensions: readonly,
    });
    refs.set(proxy, { backend, node: value.node });
    cache.set(value.node, new WeakRef(proxy));
    return proxy;
  }
  let root, dataNode, metaNode;
  const lookupRoot = key => {
    root ??= backend.root();
    return backend.lookup(root.node, key);
  };
  const data = () => dataNode ??= lookupRoot("data");
  let decoded;
  const message = Object.freeze({
    __proto__: Message.prototype,
    seq,
    time: timestampNs,
    get data() { return decoded ??= backend.materialize(data().node); },
    get meta() { return wrap(metaNode ??= lookupRoot("meta")); },
    field(key) {
      if (!validKey(key)) return undefined;
      return wrap(backend.lookup(data().node, key));
    },
  });
  messages.set(message, { backend, data });
  return message;
}

function toNative(value) {
  const message = messages.get(value);
  if (message) {
    const meta = { tags: toNative(value.tags) };
    if (value.sender !== undefined) meta.sender = value.sender;
    return { seq: value.seq, time: value.time, meta, data: message.backend.materialize(message.data().node) };
  }
  const ref = refs.get(value);
  if (ref) return ref.backend.materialize(ref.node);
  if (value === null || ["string", "number", "bigint", "boolean"].includes(typeof value)) return value;
  if (Buffer.isBuffer(value)) return Buffer.from(value);
  if (Array.isArray(value)) return value.map(toNative);
  if (value && typeof value === "object" && Object.getPrototypeOf(value) === Object.prototype) {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, toNative(item)]));
  }
  throw new TypeError("expected a Plasmite view or scalar");
}

function toJson(value) {
  const message = messages.get(value);
  if (message) {
    const tags = refs.get(value.tags);
    const sender = value.sender;
    const field = sender === undefined ? "" : `,"sender":${JSON.stringify(sender)}`;
    return `{"seq":${value.seq},"time":${JSON.stringify(timeText(value.time))},"meta":{"tags":${tags.backend.json(tags.node)}${field}},"data":${message.backend.json(message.data().node)}}`;
  }
  const ref = refs.get(value);
  if (ref) return ref.backend.json(ref.node);
  if (typeof value === "bigint") return value.toString();
  if (typeof value === "number" && !Number.isFinite(value)) throw new RangeError("JSON requires a finite number");
  if (typeof value === "number" && Object.is(value, -0)) return "-0.0";
  if (value === null || ["string", "number", "boolean"].includes(typeof value)) return JSON.stringify(value);
  if (Buffer.isBuffer(value)) return JSON.stringify(value.toString("base64"));
  if (Array.isArray(value)) return `[${value.map(toJson).join(",")}]`;
  if (value && typeof value === "object" && Object.getPrototypeOf(value) === Object.prototype) {
    return `{${Object.entries(value).map(([key, item]) => JSON.stringify(key) + ":" + toJson(item)).join(",")}}`;
  }
  throw new TypeError("expected a Plasmite view or scalar");
}

const parseJson = JSON.parse;
function createNativeMessage(owner) {
  const backend = {
    owner,
    root: () => owner.root(),
    lookup: (node, key) => owner.lookup(node, key),
    at: (node, index) => owner.at(node, index),
    has: (node, key) => owner.has(node, key),
    keys: node => owner.keys(node),
    materialize: node => owner.materialize(node, parseJson),
    json: node => owner.json(node),
    *iterate(node) {
      const cursor = owner.cursor(node);
      for (let value = cursor.nextValue(); value !== undefined; value = cursor.nextValue()) yield value;
    },
  };
  return createMessageView(backend, owner.seq, owner.time);
}

const valueReference = value => refs.get(value);
const messageReference = value => messages.get(value);
function toLite3(message) {
  const reference = messages.get(message);
  if (!reference) throw new TypeError('expected a Plasmite message view');
  const backend = reference.backend;
  return backend.encodedBytes ? backend.encodedBytes() : backend.owner.encodedBytes();
}
module.exports = { Message, createMessageView, createNativeMessage, toNative, toJson, toLite3, valueReference, messageReference };
