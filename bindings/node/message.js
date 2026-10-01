/*
Purpose: Define the canonical Node Message model shared by local and remote clients.
Key Exports: Message, parseMessage, messageFromEnvelope.
Role: Keep local/remote message shapes and parsing behavior identical.
Invariants: Message timestamps parse as valid UTC Date values.
Invariants: Message meta tags are normalized to string arrays.
Notes: Raw bytes are preserved when source buffers are available.
*/

const UINT64_MAX = (1n << 64n) - 1n;

function normalizeMessageSeq(value) {
  let seq;
  if (typeof value === "bigint") {
    seq = value;
  } else if (typeof value === "number" && Number.isSafeInteger(value)) {
    seq = BigInt(value);
  } else if (typeof value === "string" && /^[0-9]+$/.test(value)) {
    const digits = value.replace(/^0+/, "") || "0";
    if (digits.length > 20) {
      throw new RangeError("message seq must fit an unsigned 64-bit integer");
    }
    seq = BigInt(digits);
  } else {
    throw new TypeError("message seq must be an unsigned integer; use bigint or a decimal string for large values");
  }
  if (seq < 0n || seq > UINT64_MAX) {
    throw new RangeError("message seq must fit an unsigned 64-bit integer");
  }
  return seq;
}

// JSON.parse has already validated the token's syntax. Resolve its decimal
// scale exactly, without converting significant digits through Number or
// allocating zero padding from an unchecked exponent.
function sequenceFromJsonNumber(source) {
  const parts = /^(-?)([0-9]+)(?:\.([0-9]+))?(?:[eE]([+-]?[0-9]+))?$/.exec(source);
  const [, sign, integer, fraction = "", exponent = "0"] = parts;
  const digits = (integer + fraction).replace(/^0+/, "");
  if (!digits) {
    return 0n;
  }
  const scale = Number(exponent) - fraction.length;
  const integerLength = digits.length + scale;
  if (sign || !Number.isSafeInteger(scale) || integerLength > 20) {
    throw new RangeError("message seq must fit an unsigned 64-bit integer");
  }
  if (integerLength <= 0 || (scale < 0 && /[1-9]/.test(digits.slice(integerLength)))) {
    throw new TypeError("message seq must be an unsigned integer");
  }
  const integerDigits = scale < 0
    ? digits.slice(0, integerLength)
    : digits + "0".repeat(scale);
  return normalizeMessageSeq(integerDigits);
}

// Revivers visit children before their parents. Save source tokens by parent
// identity, then restore only the selected envelope's sequence after parsing.
// Numbers in the user's data and unrelated response fields keep JSON semantics.
function parseMessageJson(text, envelopeKey = null) {
  const seqSources = new WeakMap();
  const parsed = JSON.parse(text, function (key, value, context) {
    if (key === "seq" && typeof value === "number") {
      seqSources.set(this, context.source);
    }
    return value;
  });
  const envelope = envelopeKey === null ? parsed : parsed?.[envelopeKey];
  if (envelope && typeof envelope === "object" && typeof envelope.seq === "number") {
    envelope.seq = sequenceFromJsonNumber(seqSources.get(envelope));
  }
  return parsed;
}

function normalizeMessageTags(meta) {
  if (!meta || typeof meta !== "object" || !Array.isArray(meta.tags)) {
    return Object.freeze([]);
  }
  return Object.freeze(meta.tags.map((tag) => String(tag)));
}

function serializeSeq(seq) {
  const asNumber = Number(seq);
  if (Number.isSafeInteger(asNumber)) {
    return asNumber;
  }
  return seq.toString();
}

class Message {
  constructor(envelope, raw = null) {
    if (!envelope || typeof envelope !== "object") {
      throw new TypeError("message envelope must be an object");
    }
    const seq = normalizeMessageSeq(envelope.seq);
    const timeRfc3339 = String(envelope.time);
    const time = new Date(timeRfc3339);
    if (!Number.isFinite(time.getTime())) {
      throw new TypeError("message time must be RFC3339");
    }
    const tags = normalizeMessageTags(envelope.meta);
    this.seq = seq;
    this.time = time;
    this.timeRfc3339 = timeRfc3339;
    this.data = envelope.data;
    this.meta = Object.freeze({ tags });
    this._raw = Buffer.isBuffer(raw) ? raw : null;
  }

  get tags() {
    return this.meta.tags;
  }

  get raw() {
    if (!this._raw) {
      this._raw = Buffer.from(JSON.stringify({
        seq: serializeSeq(this.seq),
        time: this.timeRfc3339,
        data: this.data,
        meta: { tags: [...this.meta.tags] },
      }));
    }
    return this._raw;
  }
}

function messageFromEnvelope(envelope, raw = null) {
  return new Message(envelope, raw);
}

function parseMessage(payload) {
  if (payload instanceof Message) {
    return payload;
  }
  if (Buffer.isBuffer(payload)) {
    const parsed = parseMessageJson(payload.toString("utf8"));
    return messageFromEnvelope(parsed, payload);
  }
  if (payload && typeof payload === "object") {
    return messageFromEnvelope(payload);
  }
  throw new TypeError("payload must be Buffer, Message, or message envelope object");
}

module.exports = {
  Message,
  messageFromEnvelope,
  parseMessage,
  parseMessageJson,
};
