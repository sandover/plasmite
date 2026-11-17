const U64_MAX = (1n << 64n) - 1n;
function value(n) {
  if (typeof n !== "bigint" && !(typeof n === "number" && Number.isSafeInteger(n))) throw new TypeError("follow position must be an exact integer");
  const v = BigInt(n);
  if (v < 0n || v > U64_MAX) throw new RangeError("follow position must fit uint64");
  return v;
}
const FollowStart = Object.freeze({
  new: () => Object.freeze({kind: "new"}),
  all: () => Object.freeze({kind: "sequence", seq: 0n}),
  last: n => Object.freeze({kind: "last", count: value(n)}),
  since: n => Object.freeze({kind: "since", timestampNs: value(n)}),
  sequence: n => Object.freeze({kind: "sequence", seq: value(n)}),
});
function followStart(start = FollowStart.new()) {
  switch (start?.kind) {
    case "new": return [0, 0n];
    case "last": return [1, value(start.count)];
    case "since": return [2, value(start.timestampNs)];
    case "sequence": return [3, value(start.seq)];
    default: throw new TypeError("start must come from FollowStart");
  }
}
module.exports = {FollowStart, followStart};
