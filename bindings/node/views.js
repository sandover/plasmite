// Immutable language views. Backends own checked Lite3 bytes and return node
// descriptors for containers/bytes; scalars cross directly and undefined means absent.
const refs = new WeakMap();
const messages = new WeakMap();
const readonly = () => { throw new TypeError("Plasmite views are read-only"); };
const indexOf = key => typeof key === "string" && /^(0|[1-9][0-9]*)$/.test(key)
  && Number(key) < 0xffffffff ? Number(key) : -1;

function timestampText(ns) {
  const date = new Date(Number(ns / 1000000n));
  const fraction = (ns % 1000000000n).toString().padStart(9, "0").replace(/0+$/, "");
  return date.toISOString().slice(0, -5) + (fraction ? "." + fraction : "") + "Z";
}

function createMessageView(backend, seq, timestampNs) {
  const cache = new Map();
  function wrap(value) {
    if (value === null || typeof value !== "object") return value;
    const prior = cache.get(value.node)?.deref();
    if (prior) return prior;
    const array = value.kind === "array";
    const bytes = value.kind === "bytes";
    const target = array ? [] : Object.create(null);
    if (array || bytes) Object.defineProperty(target, "length", { value: value.count, writable: false });
    const lookup = key => {
      if (typeof key !== "string" || key.includes("\0") || !key.isWellFormed()) return undefined;
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
        return (typeof key === "string" && !key.includes("\0") && key.isWellFormed()
          && backend.has(value.node, key)) || Reflect.has(target, key);
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
  const message = Object.freeze({
    seq, timestampNs,
    get data() { return wrap(dataNode ??= lookupRoot("data")); },
    get meta() { return wrap(metaNode ??= lookupRoot("meta")); },
    get timeRfc3339() { return timestampText(timestampNs); },
  });
  messages.set(message, { backend });
  return message;
}

function toNative(value) {
  const message = messages.get(value);
  if (message) return { seq: value.seq, time: value.timeRfc3339,
    meta: { tags: toNative(value.meta.tags) }, data: toNative(value.data) };
  const ref = refs.get(value);
  if (ref) return ref.backend.materialize(ref.node);
  if (value === null || ["string", "number", "bigint", "boolean"].includes(typeof value)) return value;
  throw new TypeError("expected a Plasmite view or scalar");
}

function toJson(value) {
  const message = messages.get(value);
  if (message) {
    const data = refs.get(value.data);
    const tags = refs.get(value.meta.tags);
    return `{"seq":${value.seq},"time":${JSON.stringify(value.timeRfc3339)},"meta":{"tags":${tags.backend.json(tags.node)}},"data":${data.backend.json(data.node)}}`;
  }
  const ref = refs.get(value);
  if (ref) return ref.backend.json(ref.node);
  if (typeof value === "bigint") return value.toString();
  if (typeof value === "number" && !Number.isFinite(value)) throw new RangeError("JSON requires a finite number");
  if (typeof value === "number" && Object.is(value, -0)) return "-0.0";
  if (value === null || ["string", "number", "boolean"].includes(typeof value)) return JSON.stringify(value);
  throw new TypeError("expected a Plasmite view or scalar");
}

module.exports = { createMessageView, toNative, toJson };
