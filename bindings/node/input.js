// Private semantic helpers for the environment-thread native input walker.
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

const inputHelpers = Object.freeze({
  bufferToJSON,
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
    return { owner: ref.backend.owner, node: ref.node };
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
module.exports = { inputHelpers, snapshotTags };
