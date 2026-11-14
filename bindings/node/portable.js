// Private remote-only backend: checked shared core, ordinary JS Proxy protocols.
const fs = require('node:fs');
const path = require('node:path');
const types = require('node:util').types;
const { createMessageView, messageReference, valueReference } = require('./views');
const { inputHelpers, snapshotTags } = require('./input');

const decodeText = new TextDecoder('utf-8', { fatal: true });
const encodeText = new TextEncoder();
const decodeBytes = decodeText.decode.bind(decodeText);
const encodeInto = encodeText.encodeInto.bind(encodeText);
const parseJson = JSON.parse;
const typed = Object.getPrototypeOf(Uint8Array.prototype);
const getBuffer = Object.getOwnPropertyDescriptor(typed, 'buffer').get;
const getOffset = Object.getOwnPropertyDescriptor(typed, 'byteOffset').get;
const getLength = Object.getOwnPropertyDescriptor(typed, 'byteLength').get;
const MAX_BYTES = 256 * 1024 * 1024;
let runtime;

function core() {
  if (runtime) return runtime;
  // No await precedes input capture. A cold call pays compilation once; warm
  // callers copy directly into private linear memory before returning a Promise.
  const module = new WebAssembly.Module(fs.readFileSync(path.join(__dirname, 'portable', 'plasmite_core.wasm')));
  if (WebAssembly.Module.imports(module).length) throw new Error('portable core must have no runtime imports');
  const wasm = new WebAssembly.Instance(module, {}).exports;
  if (wasm.portable_version() !== 1) throw new Error('unsupported portable core version');
  runtime = { wasm, u8: null, data: null };
  return runtime;
}
function memory() {
  const r = core();
  if (r.u8?.buffer !== r.wasm.memory.buffer) {
    r.u8 = new Uint8Array(r.wasm.memory.buffer);
    r.data = new DataView(r.wasm.memory.buffer);
  }
  return r;
}
function text(pointer, length) {
  return decodeBytes(memory().u8.subarray(pointer, pointer + length));
}
function error() {
  const w = core().wasm;
  const message = text(w.error_ptr(), w.error_len());
  const kind = w.error_kind();
  const result = kind === 2 ? new TypeError(message) : new Error(message);
  result.kind = kind;
  return result;
}
function bytesOf(value) {
  if (!types.isUint8Array(value)) throw new TypeError('encoded input must be a Uint8Array or Buffer');
  return new Uint8Array(getBuffer.call(value), getOffset.call(value), getLength.call(value));
}
function allocate(size) {
  if (size < 1 || size > MAX_BYTES) throw new RangeError('portable input exceeds the 256 MiB core limit');
  const pointer = core().wasm.input_alloc(size);
  if (!pointer) throw new RangeError('portable memory is full');
  return pointer;
}
const releases = new FinalizationRegistry(({ kind, pointer }) => {
  core().wasm[kind === 'owner' ? 'owner_free' : 'cursor_free'](pointer);
});
function releaseOwner(owner) { PortableOwner.release(owner); }
function nativePointer(owner) { return PortableOwner.pointerOf(owner); }
function find(owner, node, key) {
  const size = stringLength(key);
  const r = core();
  const pointer = size <= 256 ? (r.lookupPointer ??= allocate(256)) : allocate(size);
  try {
    putText(key, pointer, size);
    const found = r.wasm.value_get(nativePointer(owner), node, pointer, size);
    if (found === -2) throw error();
    return found;
  } finally { if (size > 256) r.wasm.input_free(pointer, size); }
}
class Scratch {
  pointer = 0;
  capacity = 0;
  ensure(size) {
    if (size > this.capacity) {
      this.close();
      this.pointer = allocate(Math.max(size, 256));
      this.capacity = Math.max(size, 256);
    }
    return this.pointer;
  }
  close() {
    if (this.pointer) core().wasm.input_free(this.pointer, this.capacity);
    this.pointer = this.capacity = 0;
  }
}
function stringLength(value) {
  if (!value.isWellFormed()) throw new TypeError('input string contains an isolated UTF16 surrogate');
  return Buffer.byteLength(value, 'utf8');
}
function putText(value, pointer, length) {
  if (length) encodeInto(value, memory().u8.subarray(pointer, pointer + length));
}
function descriptor(owner, node) {
  const pointer = core().wasm.value_read(nativePointer(owner), node);
  if (!pointer) throw error();
  const data = memory().data;
  const kind = data.getUint32(pointer, true);
  const count = data.getUint32(pointer + 4, true);
  switch (kind) {
    case 1: return null;
    case 2: return data.getInt32(pointer + 8, true) !== 0;
    case 3: {
      // Most integers need no temporary BigInt. Only unsafe stored i64s box one.
      const number = data.getInt32(pointer + 12, true) * 4294967296 + data.getUint32(pointer + 8, true);
      return Number.isSafeInteger(number) ? number : data.getBigInt64(pointer + 8, true);
    }
    case 4: return data.getFloat64(pointer + 16, true);
    case 5: return text(data.getUint32(pointer + 24, true), count);
    case 6: return { node, kind: 'bytes', count, pointer: data.getUint32(pointer + 24, true) };
    case 7: return { node, kind: 'object', count };
    case 8: return { node, kind: 'array', count };
    default: throw new Error('invalid checked value kind');
  }
}
class Cursor {
  constructor(owner, node) {
    this.owner = owner;
    this.pointer = core().wasm.cursor_new(nativePointer(owner), node);
    if (!this.pointer) throw error();
    this.entries = core().wasm.cursor_entries(this.pointer);
    releases.register(this, { kind: 'cursor', pointer: this.pointer }, this);
  }
  *entriesOf() {
    try {
      for (;;) {
        const count = core().wasm.cursor_next(this.pointer);
        if (count < 0) throw error();
        if (count === 0) return;
        for (let i = 0; i < count; i++) {
          const offset = this.entries + i * 12;
          const data = memory().data;
          const key = data.getUint32(offset, true);
          const length = data.getUint32(offset + 4, true);
          const node = data.getUint32(offset + 8, true);
          yield [key ? text(key, length) : undefined, node];
        }
      }
    } finally {
      releases.unregister(this);
      core().wasm.cursor_free(this.pointer);
    }
  }
}
class PortableOwner {
  #pointer;
  #safe;
  static pointerOf(owner) { return owner && #pointer in owner ? owner.#pointer : undefined; }
  static release(owner) {
    if (owner.#pointer) {
      releases.unregister(owner);
      core().wasm.owner_free(owner.#pointer);
      owner.#pointer = 0;
    }
  }
  constructor(pointer) {
    this.#pointer = pointer;
    this.#safe = !!core().wasm.owner_safe(pointer);
    releases.register(this, { kind: 'owner', pointer }, this);
  }
  root() { return descriptor(this, 0); }
  lookup(node, key) {
    const found = find(this, node, key);
    return found === -1 ? undefined : descriptor(this, found);
  }
  has(node, key) { return find(this, node, key) !== -1; }
  at(node, index) {
    const info = descriptor(this, node);
    if (info.kind === 'bytes') return memory().u8[info.pointer + index];
    const found = core().wasm.value_index(this.#pointer, node, index);
    if (found === -2) throw error();
    return found === -1 ? undefined : descriptor(this, found);
  }
  keys(node) { return Array.from(new Cursor(this, node).entriesOf(), entry => entry[0]); }
  *iterate(node) {
    const info = descriptor(this, node);
    if (info.kind === 'bytes') {
      for (let i = 0; i < info.count; i++) yield memory().u8[info.pointer + i];
    } else {
      for (const [, child] of new Cursor(this, node).entriesOf()) yield descriptor(this, child);
    }
  }
  json(node, stored = false) {
    const w = core().wasm;
    if (w.value_json(this.#pointer, node, stored) < 0) throw error();
    try { return text(w.output_ptr(), w.output_len()); } finally { w.output_free(); }
  }
  materialize(node) {
    if (this.#safe) return parseJson(this.json(node, true));
    const value = descriptor(this, node);
    if (value === null || typeof value !== 'object') return value;
    if (value.kind === 'bytes') {
      const output = Buffer.allocUnsafeSlow(value.count);
      output.set(memory().u8.subarray(value.pointer, value.pointer + value.count));
      return output;
    }
    const output = value.kind === 'array' ? [] : {};
    for (const [key, child] of new Cursor(this, node).entriesOf()) {
      const copy = this.materialize(child);
      if (value.kind === 'array') output.push(copy);
      else Object.defineProperty(output, key, { value: copy, enumerable: true, writable: true, configurable: true });
    }
    return output;
  }
  encodedBytes() {
    const w = core().wasm;
    const length = w.owner_len(this.#pointer);
    const pointer = w.owner_ptr(this.#pointer);
    const output = Buffer.allocUnsafeSlow(length);
    output.set(memory().u8.subarray(pointer, pointer + length));
    return output;
  }
}
function backend(owner) {
  return {
    owner, root: () => owner.root(), lookup: (node, key) => owner.lookup(node, key),
    at: (node, index) => owner.at(node, index), has: (node, key) => owner.has(node, key),
    keys: node => owner.keys(node), iterate: node => owner.iterate(node),
    materialize: node => owner.materialize(node), json: node => owner.json(node),
    encodedBytes: () => owner.encodedBytes(),
  };
}
function decodeOwner(payload) {
  const bytes = bytesOf(payload);
  const pointer = bytes.byteLength ? allocate(bytes.byteLength) : 0;
  try { if (bytes.byteLength) memory().u8.set(bytes, pointer); } catch (err) {
    core().wasm.input_free(pointer, bytes.byteLength);
    throw err;
  }
  // The core consumes input_alloc's allocation even when validation fails.
  const owner = core().wasm.owner_decode(pointer, bytes.byteLength);
  if (!owner) throw error();
  return new PortableOwner(owner);
}
function identity(value, name) {
  if (typeof value !== 'bigint' || value < 0n || value > 0xffffffffffffffffn) throw new TypeError(`${name} must be an unsigned64 bigint`);
  return value;
}
async function decodeMessage(payload, seq, timestampNs) {
  identity(seq, 'seq'); identity(timestampNs, 'timestampNs');
  return createMessageView(backend(decodeOwner(payload)), seq, timestampNs);
}
function encoded(reference) {
  const owner = inputHelpers.existingOwner?.(reference.backend) ?? reference.backend.owner;
  if (owner?.encodedBytes) return owner.encodedBytes();
  return reference.backend.encodedBytes();
}
function normalize(value, key) {
  let type = typeof value;
  let kind = value !== null && type === 'object' ? inputHelpers.classify(value) : 0;
  if (kind === 13) return [value, kind];
  if (value !== null && ['object', 'function', 'bigint'].includes(type)) {
    const hook = type === 'bigint' ? inputHelpers.bigintHook(value) : value.toJSON;
    if (typeof hook === 'function' && !(kind === 6 && hook === inputHelpers.bufferToJSON)) {
      value = Reflect.apply(hook, value, [typeof key === 'number' ? String(key) : key]);
      type = typeof value;
      kind = value !== null && type === 'object' ? inputHelpers.classify(value) : 0;
    }
    if (kind >= 9 && kind <= 12) { value = inputHelpers.unbox(value, kind); kind = 0; }
  }
  return [value, kind];
}
class Encoder {
  scratch = new Scratch();
  imports = new Map();
  stack = [];
  constructor(tags) {
    const sizes = tags.map(stringLength);
    const total = tags.length * 8 + sizes.reduce((a, b) => a + b, 0);
    const pointer = total ? this.scratch.ensure(total) : 0;
    let position = pointer + tags.length * 8;
    for (let i = 0; i < tags.length; i++) {
      const data = memory().data;
      data.setUint32(pointer + i * 8, position, true);
      data.setUint32(pointer + i * 8 + 4, sizes[i], true);
      putText(tags[i], position, sizes[i]); position += sizes[i];
    }
    this.pointer = core().wasm.builder_new(pointer, tags.length);
    if (!this.pointer) { this.scratch.close(); throw error(); }
    this.data = core().wasm.builder_data();
  }
  close() {
    if (this.pointer) core().wasm.plsm_builder_free(this.pointer);
    this.pointer = 0;
    this.scratch.close();
    for (const owner of this.imports.values()) releaseOwner(owner);
  }
  source(reference) {
    const original = reference.backend.owner;
    if (nativePointer(original) !== undefined) return original;
    let owner = this.imports.get(reference.backend);
    if (!owner) { owner = decodeOwner(encoded(reference)); this.imports.set(reference.backend, owner); }
    return owner;
  }
  copy(parent, key, reference) {
    const owner = this.source(reference);
    const size = key === null ? 0 : Buffer.byteLength(key, 'utf8');
    const pointer = size ? this.scratch.ensure(size) : (key === null ? 0 : 1);
    if (size) putText(key, pointer, size);
    if (core().wasm.builder_copy(this.pointer, parent, pointer, size, nativePointer(owner), reference.node) < 0) throw error();
  }
  put(parent, key, kind, value) {
    const keySize = key === null ? 0 : Buffer.byteLength(key, 'utf8');
    const size = kind === 5 ? stringLength(value) : kind === 6 ? value.byteLength : 0;
    const pointer = (keySize + size) ? this.scratch.ensure(keySize + size) : 0;
    const keyPointer = key === null ? 0 : keySize ? pointer : 1;
    if (keySize) putText(key, pointer, keySize);
    const valuePointer = pointer + keySize;
    if (kind === 5) putText(value, valuePointer, size);
    if (kind === 6) memory().u8.set(value, valuePointer);
    let low = 0, high = 0;
    if (kind === 3) {
      if (typeof value === 'bigint') {
        low = Number(BigInt.asUintN(32, value)); high = Number(value >> 32n);
      } else { low = value >>> 0; high = Math.floor(value / 4294967296); }
    } else if (kind === 2) low = value ? 1 : 0;
    const child = core().wasm.builder_put(this.pointer, parent, keyPointer, keySize, kind,
      low, high, kind === 4 ? value : 0, valuePointer, size);
    if (child === -1) throw error();
    return child;
  }
  members(parent, value, kind) {
    if (this.stack.includes(value)) throw new TypeError('cannot encode a cyclic input');
    if (this.stack.length >= 31) throw new TypeError('input exceeds Lite3 nesting depth');
    this.stack.push(value);
    try {
      if (kind === 8) {
        const numeric = +value.length;
        const length = Number.isNaN(numeric) || numeric < 0 ? 0 : Math.min(Math.floor(numeric), Number.MAX_SAFE_INTEGER);
        for (let i = 0; i < length; i++) this.value(parent, null, i, value[i], true);
      } else {
        const keys = Object.keys(value);
        for (const key of keys) this.value(parent, key, key, value[key], false);
      }
    } finally { this.stack.pop(); }
  }
  value(parent, key, hookKey, value, array) {
    // Argument evaluation has already performed Get; reject malformed keys
    // before the child can run toJSON, matching the native environment walker.
    if (key !== null && !key.isWellFormed()) throw new TypeError('input key contains an isolated UTF16 surrogate');
    let item = value, kind = 0;
    if (value !== null && ['object', 'function', 'bigint'].includes(typeof value)) {
      [item, kind] = normalize(value, hookKey);
    }
    if (kind === 13) return this.copy(parent, key, valueReference(item));
    const type = typeof item;
    if (['undefined', 'function', 'symbol'].includes(type)) {
      if (array) this.put(parent, key, 1, null);
    } else if (item === null) this.put(parent, key, 1, null);
    else if (type === 'boolean') this.put(parent, key, 2, item);
    else if (type === 'bigint') {
      if (item < -(1n << 63n) || item >= 1n << 63n) throw new TypeError('bigint must fit signed i64');
      this.put(parent, key, 3, item);
    } else if (type === 'number') {
      if (!Number.isFinite(item)) throw new TypeError('number must be finite');
      this.put(parent, key, Number.isSafeInteger(item) && !Object.is(item, -0) ? 3 : 4, item);
    } else if (type === 'string') this.put(parent, key, 5, item);
    else if (kind === 6) {
      const bytes = bytesOf(item);
      if (types.isSharedArrayBuffer(bytes.buffer)) throw new TypeError('shared-backed bytes require an ordinary buffer copy');
      this.put(parent, key, 6, bytes);
    } else {
      const child = this.put(parent, key, kind, null);
      this.members(child, item, kind);
    }
  }
  finish(data) {
    const ref = valueReference(data);
    if (ref) this.copy(-1, null, ref);
    else {
      const [value, kind] = normalize(data, '');
      if (kind === 13) this.copy(-1, null, valueReference(value));
      else {
        if (value === null || typeof value !== 'object' || kind !== 7) throw new TypeError('message data must be an object');
        this.members(this.data, value, kind);
      }
    }
    const pointer = core().wasm.builder_finish(this.pointer);
    if (!pointer) throw error();
    return new PortableOwner(pointer);
  }
}
function encodeOwner(data, tags) {
  const encoder = new Encoder(snapshotTags(tags));
  try {
    const message = messageReference(data);
    return encoder.finish(message ? data.data : data);
  } finally { encoder.close(); }
}
async function encodeMessage(data, tags) {
  const message = messageReference(data);
  if (message && tags === undefined) return encoded(message);
  const owner = encodeOwner(data, tags);
  try { return owner.encodedBytes(); } finally { releaseOwner(owner); }
}
// A failed request discards only a newly built, unpublished owner. Forwarded
// caller views stay alive; a successful request retains this same backend.
function prepareAppend(data, tags) {
  const reference = messageReference(data);
  if (reference && tags === undefined) {
    return { payload: encoded(reference), message: data, discard() {} };
  }
  const owner = encodeOwner(data, tags);
  try {
    return { payload: owner.encodedBytes(),
      message: createMessageView(backend(owner), 0n, 0n),
      discard() { releaseOwner(owner); } };
  } catch (err) { releaseOwner(owner); throw err; }
}
function messageWithIdentity(message, seq, timestampNs) {
  const reference = messageReference(message);
  if (!reference) throw new TypeError('expected a Plasmite MessageView');
  identity(seq, 'seq'); identity(timestampNs, 'timestampNs');
  return createMessageView(reference.backend, seq, timestampNs);
}
// Diagnostics for checked package tests/benchmarks, not a selectable backend API.
function diagnostics() {
  if (!runtime) return { initialized: false, linearBytes: 0, ownerCount: 0, backingBytes: 0 };
  return { initialized: true, linearBytes: core().wasm.memory.buffer.byteLength,
    ownerCount: core().wasm.live_owners(), backingBytes: core().wasm.live_bytes(),
    lookupWorkspaceBytes: runtime.lookupPointer ? 256 : 0 };
}
module.exports = { decodeMessage, encodeMessage, prepareAppend, messageWithIdentity, diagnostics };
