//! Purpose: Regression coverage for parse-failure category mapping.
//! Exports: Integration tests only.
//! Role: Verify stable category labels used by runtime parse diagnostics.
//! Invariants: Category mapping remains deterministic for representative errors.
//! Invariants: Tests avoid payload leakage; assertions target category/hint text only.
//! Notes: Uses source include to exercise internal helper logic without widening API surface.

#[path = "../src/json/parse.rs"]
mod parse;

use parse::ParseFailureCategory;
use serde_json::Value;

#[test]
fn category_mapping_handles_syntax_and_numeric_errors() {
    let syntax_err = parse::from_str::<Value>(r#"{"a":}"#).unwrap_err();
    assert_eq!(
        parse::categorize_error(&syntax_err),
        ParseFailureCategory::Syntax
    );

    let mut number_bytes = br#"{"n":18446744073709551616}"#.to_vec();
    let number_err = simd_json::serde::from_slice::<Value>(&mut number_bytes).unwrap_err();
    assert_eq!(
        parse::categorize_error(&number_err),
        ParseFailureCategory::NumericRange
    );
}

#[test]
fn category_mapping_handles_utf8_and_depth_messages() {
    let utf8_bytes = [0xff, b'{', b'}'];
    let mut utf8_vec = utf8_bytes.to_vec();
    let utf8_err = simd_json::serde::from_slice::<Value>(&mut utf8_vec).unwrap_err();
    assert_eq!(
        parse::categorize_error(&utf8_err),
        ParseFailureCategory::Utf8
    );

    assert_eq!(
        parse::categorize_message("recursion limit exceeded while parsing"),
        ParseFailureCategory::DepthLimit
    );
}

#[test]
fn hint_contains_category_and_context() {
    let mut bytes = br#"{"n":18446744073709551616}"#.to_vec();
    let err = simd_json::serde::from_slice::<Value>(&mut bytes).unwrap_err();
    let hint = parse::hint_for_error(&err, "test.context");
    assert!(hint.contains("parse category: numeric-range"));
    assert!(hint.contains("context: test.context"));
}

#[test]
fn unknown_category_fallback_is_stable() {
    assert_eq!(
        parse::categorize_message("opaque parser issue"),
        ParseFailureCategory::Unknown
    );
}
