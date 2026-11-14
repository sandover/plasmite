// Shared input semantics and private environment-thread writer marshalling.
const types = require('node:util').types;
const { valueReference } = require('./views');
const stringValue = String;
const apply = Reflect.apply;
const booleanValue = Boolean.prototype.valueOf;
const bigintValue = BigInt.prototype.valueOf;
const bufferToJSON = Buffer.prototype.toJSON;
const bufferPrototype = Buffer.prototype;
const bytePrototype = Uint8Array.prototype;
const isRawJSON = JSON.isRawJSON;
const ownKeys = Object.keys;
const setPrototype = Object.setPrototypeOf, createObject = Object.create;
const hasNul = Function.prototype.call.bind(String.prototype.includes);
const isWellFormed = Function.prototype.call.bind(String.prototype.isWellFormed);
const finiteNumber = Number.isFinite, safeInteger = Number.isSafeInteger, nanNumber = Number.isNaN;
const sameValue = Object.is, floorNumber = Math.floor, minNumber = Math.min;
const U32 = Uint32Array, F64 = Float64Array, I64 = BigInt64Array, Bytes = Uint8Array, BufferStorage = ArrayBuffer;
function privateArray() { return setPrototype([], null); }
function privateKeys() { return createObject(null); }
let importNative, NativeViewType;
const importedOwners = new WeakMap();
function configureNativeInput(importer, NativeView) { importNative = importer; NativeViewType = NativeView; }
function nativeOwner(backend) {
  if (NativeViewType && backend.owner instanceof NativeViewType) return backend.owner;
  let owner = importedOwners.get(backend);
  if (!owner) {
    if (!importNative) throw new Error('native views are unavailable');
    const bytes = backend.encodedBytes ? backend.encodedBytes() : backend.owner.encodedBytes();
    const buffer = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    owner = importNative(buffer);
    importedOwners.set(backend, owner);
  }
  return owner;
}

const inputHelpers = Object.freeze({
  bufferToJSON,
  existingOwner(backend) { return backend.owner || importedOwners.get(backend); },
  toJSONKey: 'toJSON',
  lengthKey: 'length',
  bigintHook(value) { return value.toJSON; },
  classify(value) {
    if (isRawJSON(value)) throw new TypeError('JSON.rawJSON requires the explicit JSON input boundary');
    if (valueReference(value)) return 13;
    if (Array.isArray(value)) return 8;
    if (types.isUint8Array(value)) {
      const prototype = Object.getPrototypeOf(value);
      if (prototype === bufferPrototype || prototype === bytePrototype) return 6;
    }
    if (types.isBoxedPrimitive(value)) {
      if (types.isNumberObject(value)) return 9;
      if (types.isStringObject(value)) return 10;
      if (types.isBooleanObject(value)) return 11;
      if (types.isBigIntObject(value)) return 12;
    }
    return 7;
  },
  unbox(value, kind) {
    if (kind === 9) return +value;
    if (kind === 10) return stringValue(value);
    if (kind === 11) return apply(booleanValue, value, []);
    return apply(bigintValue, value, []);
  },
  view(value) {
    const ref = valueReference(value);
    return { owner: nativeOwner(ref.backend), node: ref.node };
  },
});

function snapshotTags(tags) {
  if (tags === undefined) return [];
  if (!Array.isArray(tags)) throw new TypeError('tags must be an array of strings');
  return Array.from(tags, tag => {
    if (typeof tag !== 'string' || !tag.isWellFormed()) throw new TypeError('tags must contain well-formed strings');
    return tag;
  });
}
module.exports = { inputHelpers, snapshotTags, configureNativeInput, nativeOwner };

// One semantic traversal feeds either the portable core or the native writer.
const typedPrototype = Object.getPrototypeOf(Uint8Array.prototype);
const getBuffer = Object.getOwnPropertyDescriptor(typedPrototype, 'buffer').get;
const getOffset = Object.getOwnPropertyDescriptor(typedPrototype, 'byteOffset').get;
const getLength = Object.getOwnPropertyDescriptor(typedPrototype, 'byteLength').get;
function bytesOf(value) {
  if (!types.isUint8Array(value)) throw new TypeError('encoded input must be a Uint8Array or Buffer');
  return new Bytes(getBuffer.call(value), getOffset.call(value), getLength.call(value));
}
function normalize(walker, value, key) {
  let type = typeof value;
  let kind = value !== null && type === 'object' ? inputHelpers.classify(value) : 0;
  if (kind === 13) { walker.normalKind = kind; return value; }
  if (value !== null && (type === 'object' || type === 'function' || type === 'bigint')) {
    const hook = type === 'bigint' ? inputHelpers.bigintHook(value) : value.toJSON;
    if (typeof hook === 'function' && !(kind === 6 && hook === inputHelpers.bufferToJSON)) {
      value = apply(hook, value, [typeof key === 'number' ? stringValue(key) : key]);
      type = typeof value;
      kind = value !== null && type === 'object' ? inputHelpers.classify(value) : 0;
    }
    if (kind >= 9 && kind <= 12) { value = inputHelpers.unbox(value, kind); kind = 0; }
  }
  walker.normalKind = kind;
  return value;
}
class InputWalker {
  normalKind = 0;
  stack = privateArray();
  members(parent, value, kind) {
    for (let i = 0; i < this.stack.length; i++) if (this.stack[i] === value) throw new TypeError('cannot encode a cyclic input');
    if (this.stack.length >= 31) throw new TypeError('input exceeds Lite3 nesting depth');
    this.stack[this.stack.length] = value;
    try {
      if (kind === 8) {
        const numeric = +value.length;
        const length = nanNumber(numeric) || numeric < 0 ? 0 : minNumber(floorNumber(numeric), 9007199254740991);
        for (let i = 0; i < length; i++) this.value(parent, null, i, value[i], true);
      } else {
        const keys = ownKeys(value);
        for (let i = 0; i < keys.length; i++) { const key = keys[i]; this.value(parent, key, key, value[key], false); }
      }
    } finally { this.stack.length--; }
  }
  value(parent, key, hookKey, value, array) {
    // Argument evaluation has already performed Get; reject malformed keys
    // before the child can run toJSON, matching the native environment walker.
    if (key !== null && !isWellFormed(key)) throw new TypeError('input key contains an isolated UTF16 surrogate');
    let item = value, kind = 0;
    if (value !== null && (typeof value === 'object' || typeof value === 'function' || typeof value === 'bigint')) {
      item = normalize(this, value, hookKey); kind = this.normalKind;
    }
    if (kind === 13) return this.copy(parent, key, valueReference(item));
    const type = typeof item;
    if ((type === 'undefined' || type === 'function' || type === 'symbol')) {
      if (array) this.put(parent, key, 1, null);
    } else if (item === null) this.put(parent, key, 1, null);
    else if (type === 'boolean') this.put(parent, key, 2, item);
    else if (type === 'bigint') {
      if (item < -(1n << 63n) || item >= 1n << 63n) throw new TypeError('bigint must fit signed i64');
      this.put(parent, key, 3, item);
    } else if (type === 'number') {
      if (!finiteNumber(item)) throw new TypeError('number must be finite');
      this.put(parent, key, safeInteger(item) && !sameValue(item, -0) ? 3 : 4, item);
    } else if (type === 'string') {
      if (!isWellFormed(item)) throw new TypeError('input string contains an isolated UTF16 surrogate');
      this.put(parent, key, 5, item);
    }
    else if (kind === 6) {
      const bytes = bytesOf(item);
      if (types.isSharedArrayBuffer(getBuffer.call(bytes))) throw new TypeError('shared-backed bytes require an ordinary buffer copy');
      this.put(parent, key, 6, bytes);
    } else {
      const child = this.put(parent, key, kind, null);
      this.members(child, item, kind);
      this.endContainer(child);
    }
  }
  write(data) {
    const ref = valueReference(data);
    if (ref) this.copy(-1, null, ref);
    else {
      const value = normalize(this, data, '');
      const kind = this.normalKind;
      if (kind === 13) this.copy(-1, null, valueReference(value));
      else {
        if (value === null || typeof value !== 'object' || kind !== 7) throw new TypeError('message data must be an object');
        this.members(this.data, value, kind);
      }
    }
  }
  endContainer() {}
}
module.exports.InputWalker = InputWalker;
module.exports.bytesOf = bytesOf;

// Private bounded marshalling storage, not a serialized message format. Native
// writes consume it synchronously; reentrant captures get separate storage.
const COMMANDS = 512;
// Kinds1–8 are the existing writer kinds. These two operations carry only
// traversal state and a safe JavaScript Number that the writer stores as i64.
const END_CONTAINER = 9, INTEGER_NUMBER = 10;
let idlePacket;
function packet() {
  if (idlePacket) { const result = idlePacket; idlePacket = undefined; return result; }
  const scalars = new BufferStorage(COMMANDS * 8);
  return { ops: new U32(COMMANDS * 3), numbers: new F64(scalars),
    integers: new I64(scalars), strings: privateArray(), keys: privateKeys() };
}
class NativeInput extends InputWalker {
  data = 0;
  count = 0;
  constructor(target, tags) { super(); this.target = target; this.tags = tags; this.started = false; this.packet = packet(); }
  key(value) {
    if (value === null) return 0xffffffff;
    const p = this.packet;
    let index = p.keys[value];
    if (index === undefined) { index = p.strings.length; p.strings[p.strings.length] = value; p.keys[value] = index; }
    return index;
  }
  flush(required = false) {
    if (!this.count && (!required || this.started)) return;
    const p = this.packet;
    this.target.write(p.ops, p.numbers, p.strings, this.count, this.started ? undefined : this.tags);
    this.started = true;
    this.count = 0; p.strings.length = 0; p.keys = privateKeys();
  }
  finish() {
    const p = this.packet;
    return this.target.finish(p.ops, p.numbers, p.strings, this.count, this.started ? undefined : this.tags);
  }
  put(parent, key, kind, value) {
    if (kind === 6) {
      // The mutable bytes must reach their final encoding before later hooks.
      this.flush(true); this.target.writeBytes(key, value); return 0;
    }
    if (this.count === COMMANDS) this.flush();
    const p = this.packet, row = this.count++, position = row * 3;
    p.ops[position] = kind === 3 && typeof value === 'number' ? INTEGER_NUMBER : kind;
    p.ops[position + 1] = this.key(key);
    p.ops[position + 2] = 0;
    if (kind === 2) p.ops[position + 2] = value ? 1 : 0;
    else if (kind === 3 && typeof value === 'bigint') p.integers[row] = value;
    else if (kind === 3 || kind === 4) p.numbers[row] = value;
    else if (kind === 5) { p.ops[position + 2] = p.strings.length; p.strings[p.strings.length] = value; }
    // Preserve native failure order before descending or invoking later hooks.
    if ((key !== null && hasNul(key,'\0')) || ((kind === 7 || kind === 8) && this.stack.length >= 31)) this.flush();
    return 0;
  }
  endContainer() {
    if (this.count === COMMANDS) this.flush();
    const position = this.count++ * 3, p = this.packet;
    p.ops[position] = END_CONTAINER; p.ops[position + 1] = 0xffffffff; p.ops[position + 2] = 0;
  }
  copy(parent, key, reference) {
    // Copy checked subtrees at the visit: depth/key failures must precede the
    // next getter, and no temporary native object tree is needed.
    this.flush(true);
    this.target.writeCopy(nativeOwner(reference.backend), reference.node, key, parent === -1);
  }
  close() {
    this.packet.strings.length = 0; this.packet.keys = privateKeys();
    if (!idlePacket) idlePacket = this.packet;
    this.packet = undefined;
  }
}
function captureNativeInput(target, data, tags) {
  const input = new NativeInput(target, tags);
  try { input.write(data); return input.finish(); }
  finally { input.close(); }
}
module.exports.captureNativeInput = captureNativeInput;
