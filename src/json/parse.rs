//! Purpose: Provide the internal runtime JSON decode entrypoints.
//! Exports: `from_str`.
//! Role: Parser boundary that centralizes simd-json usage details.
//! Invariants: Decoding uses simd-json for runtime paths migrated to this boundary.
//! Invariants: Input buffers are copied once to satisfy simd-json mutable-slice API.
//! Notes: Error mapping is done by callsites so domain context stays explicit.

use serde::de::DeserializeOwned;

pub(crate) fn from_str<T: DeserializeOwned>(input: &str) -> Result<T, simd_json::Error> {
    let mut bytes = input.as_bytes().to_vec();
    simd_json::serde::from_slice(&mut bytes)
}
