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
});

const DURABILITY_VALUES = Object.freeze({
  fast: "fast",
  flush: "flush",
  0: "fast",
  1: "flush",
});

function mapErrorKind(value, fallback = undefined) {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "string") {
    const mapped = ERROR_KIND_VALUES[value];
    if (mapped !== undefined) {
      return mapped;
    }
  }
  return fallback;
}

function mapDurability(value) {
  if (value === undefined || value === null) {
    return "fast";
  }
  const mapped = DURABILITY_VALUES[String(value).toLowerCase()];
  if (mapped) {
    return mapped;
  }
  throw new TypeError("durability must be Durability.Fast or Durability.Flush");
}

module.exports = {
  ERROR_KIND_VALUES,
  mapErrorKind,
  mapDurability,
};
