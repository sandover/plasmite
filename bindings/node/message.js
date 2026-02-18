/*
Purpose: Define the canonical Node Message model shared by local and remote clients.
Key Exports: Message, parseMessage, messageFromEnvelope.
Role: Keep local/remote message shapes and parsing behavior identical.
Invariants: Message timestamps parse as valid UTC Date values.
Invariants: Message meta tags are normalized to string arrays.
Notes: Raw bytes are preserved when source buffers are available.
*/

function normalizeMessageSeq(value) {
  if (typeof value === "bigint") {
    return value;
  }
  if (typeof value === "number" && Number.isFinite(value)) {
    return BigInt(Math.trunc(value));
  }
  if (typeof value === "string" && value.length) {
    return BigInt(value);
  }
  throw new TypeError("message seq must be numeric");
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
    const parsed = JSON.parse(payload.toString("utf8"));
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
};
