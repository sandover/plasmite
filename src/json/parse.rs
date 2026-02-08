//! Purpose: Provide the internal runtime JSON decode entrypoints.
//! Exports: `from_str`, parse-failure categorization helpers.
//! Role: Parser boundary that centralizes simd-json usage details.
//! Invariants: Decoding uses simd-json for runtime paths migrated to this boundary.
//! Invariants: Input buffers are copied once to satisfy simd-json mutable-slice API.
//! Notes: Error mapping is done by callsites so domain context stays explicit.

use serde::de::DeserializeOwned;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParseFailureCategory {
    Syntax,
    Utf8,
    NumericRange,
    DepthLimit,
    Unknown,
}

pub(crate) fn category_label(category: ParseFailureCategory) -> &'static str {
    match category {
        ParseFailureCategory::Syntax => "syntax",
        ParseFailureCategory::Utf8 => "utf8",
        ParseFailureCategory::NumericRange => "numeric-range",
        ParseFailureCategory::DepthLimit => "depth-limit",
        ParseFailureCategory::Unknown => "unknown",
    }
}

pub(crate) fn categorize_message(message: &str) -> ParseFailureCategory {
    let lower = message.to_ascii_lowercase();
    if lower.contains("utf") || lower.contains("unicode") {
        return ParseFailureCategory::Utf8;
    }
    if lower.contains("invalidnumber")
        || lower.contains("out of range")
        || (lower.contains("number") && lower.contains("invalid"))
    {
        return ParseFailureCategory::NumericRange;
    }
    if lower.contains("recursion")
        || lower.contains("depth")
        || lower.contains("nest")
        || lower.contains("too deep")
    {
        return ParseFailureCategory::DepthLimit;
    }
    if lower.contains("expected ")
        || lower.contains("expected:")
        || lower.contains("at character")
        || lower.contains("trailing")
        || lower.contains("eof")
        || lower.contains("syntax")
    {
        return ParseFailureCategory::Syntax;
    }
    ParseFailureCategory::Unknown
}

pub(crate) fn categorize_error(error: &simd_json::Error) -> ParseFailureCategory {
    categorize_message(&error.to_string())
}

pub(crate) fn hint_for_error(error: &simd_json::Error, context: &str) -> String {
    let category = category_label(categorize_error(error));
    format!("parse category: {category}; context: {context}")
}

pub(crate) fn from_str<T: DeserializeOwned>(input: &str) -> Result<T, simd_json::Error> {
    let mut bytes = input.as_bytes().to_vec();
    simd_json::serde::from_slice(&mut bytes)
}
