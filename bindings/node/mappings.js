/*
Purpose: Centralize Node binding value mappings shared across local and remote paths.
Key Exports: ERROR_KIND_VALUES, mapErrorKind, mapDurability.
Role: Keep error-kind and durability normalization behavior consistent.
Invariants: Error kind names map to stable numeric values for v0 semantics.
Invariants: Durability accepts fast/flush and numeric enum aliases 0/1.
*/

const ERROR_KIND_VALUES = Object.freeze({
  Internal: 1,
  Usage: 2,
  NotFound: 3,
  AlreadyExists: 4,
  Busy: 5,
  Permission: 6,
  Corrupt: 7,
  Io: 8,
  RetentionGap: 9,
});

const DURABILITY_VALUES = Object.freeze({
  fast: "fast",
  flush: "flush",
  0: "fast",
  1: "flush",
});

function mapErrorKind(value, fallback = undefined) {
  if (typeof value === "number" && Object.values(ERROR_KIND_VALUES).includes(value)) {
    return value;
  }
  if (typeof value === "string" && Object.hasOwn(ERROR_KIND_VALUES, value)) {
    return ERROR_KIND_VALUES[value];
  }
  return fallback;
}

function mapDurability(value) {
  if (value === undefined || value === null) {
    return "fast";
  }
  const key = String(value).toLowerCase();
  if (Object.hasOwn(DURABILITY_VALUES, key)) {
    return DURABILITY_VALUES[key];
  }
  throw new TypeError("durability must be Durability.Fast or Durability.Flush");
}

module.exports = {
  ERROR_KIND_VALUES,
  mapErrorKind,
  mapDurability,
};
