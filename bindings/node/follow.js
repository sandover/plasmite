const U64_MAX = (1n << 64n) - 1n;
function value(n) {
  if (typeof n !== "bigint" && !(typeof n === "number" && Number.isSafeInteger(n))) throw new TypeError("follow position must be an exact integer");
  const v = BigInt(n);
  if (v < 0n || v > U64_MAX) throw new RangeError("follow position must fit uint64");
  return v;
}
const INSTANT = Symbol("Plasmite instant");
function at(time) {
  return Object.freeze({ [INSTANT]: value(time) });
}

const DURATION_UNITS = { ns: 1n, us: 1000n, ms: 1000000n, s: 10n ** 9n,
  m: 60n * 10n ** 9n, h: 3600n * 10n ** 9n, d: 86400n * 10n ** 9n };

function readStart(since, last) {
  if (last !== undefined) {
    if (since !== undefined) throw new TypeError("since and last are mutually exclusive");
    return [5, value(last)];
  }
  if (since === undefined) return [0, 0n];
  if (since === "oldest") return [1, 0n];
  if (typeof since === "number" || typeof since === "bigint") return [2, value(since)];
  if (typeof since === "string") {
    const text = since.trim();
    const match = /^([0-9]+)(ns|us|ms|s|m|h|d)$/i.exec(text);
    const { parseTimeNs } = require("./message");
    const time = match ? BigInt(match[1]) * DURATION_UNITS[match[2].toLowerCase()] : parseTimeNs(text);
    return [match ? 4 : 3, time < 0n ? 0n : time > U64_MAX ? U64_MAX : time];
  }
  if (since?.[INSTANT] !== undefined) return [3, since[INSTANT]];
  throw new TypeError("since must be oldest, a sequence, an instant or a duration");
}

function normalizeWait(wait) {
  const ms = Math.ceil(wait * 1000);
  if (typeof wait !== "number" || wait < 0 || !Number.isSafeInteger(ms)) {
    throw new TypeError("wait must be a non-negative duration in seconds");
  }
  return ms;
}

module.exports = {at, readStart, normalizeWait};
