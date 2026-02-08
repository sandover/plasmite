//! Purpose: Lock parser contract expectations with corpus + differential coverage.
//! Exports: Integration tests only (no runtime exports).
//! Role: Catch semantic drift between simd-json runtime parsing and serde_json baseline.
//! Invariants: Cases from docs/decisions/simd-json-parser-contract.md stay represented.
//! Invariants: Differential checks assert parity where behavior should match today.
//! Notes: Duplicate-key behavior is asserted as current parser parity, pending policy finalization.

use serde_json::Value;

fn parse_simd_json(input: &[u8]) -> Result<Value, String> {
    let mut bytes = input.to_vec();
    simd_json::serde::from_slice::<Value>(&mut bytes).map_err(|err| err.to_string())
}

fn parse_serde_json(input: &[u8]) -> Result<Value, String> {
    serde_json::from_slice::<Value>(input).map_err(|err| err.to_string())
}

fn assert_differential_parity(input: &[u8]) {
    let simd = parse_simd_json(input);
    let serde = parse_serde_json(input);
    match (simd, serde) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "parser value mismatch"),
        (Err(_), Err(_)) => {}
        (left, right) => panic!("parser outcome mismatch: simd={left:?}, serde={right:?}"),
    }
}

#[test]
fn corpus_valid_payloads_match_serde() {
    let corpus = [
        br#"{"a":1,"b":"ok"}"#.as_slice(),
        br#"[1,2,3,{"x":true}]"#.as_slice(),
        br#"{"nested":{"arr":[{"k":"v"}]}}"#.as_slice(),
        br#"{"unicode":"\u2603"}"#.as_slice(),
    ];

    for case in corpus {
        assert_differential_parity(case);
    }
}

#[test]
fn corpus_duplicate_keys_matches_current_behavior() {
    let input = br#"{"a":1,"a":2}"#;
    assert_differential_parity(input);
}

#[test]
fn corpus_malformed_utf8_rejected() {
    let bad_utf8 = [0xff, 0xfe, b'{', b'}'];
    let simd = parse_simd_json(&bad_utf8);
    let serde = parse_serde_json(&bad_utf8);
    assert!(simd.is_err(), "simd-json should reject malformed utf8");
    assert!(serde.is_err(), "serde_json should reject malformed utf8");
}

#[test]
fn corpus_deep_nesting_matches_serde() {
    let depth = 256usize;
    let mut payload = String::with_capacity(depth * 2 + 1);
    for _ in 0..depth {
        payload.push('[');
    }
    payload.push('0');
    for _ in 0..depth {
        payload.push(']');
    }
    let simd = parse_simd_json(payload.as_bytes());
    let serde = parse_serde_json(payload.as_bytes());
    assert!(
        simd.is_ok(),
        "simd-json current runtime path unexpectedly rejected deep nesting"
    );
    assert!(
        serde.is_err(),
        "serde_json baseline unexpectedly accepted deep nesting beyond recursion limit"
    );
}

#[test]
fn corpus_large_number_edges() {
    let max_u64 = br#"{"n":18446744073709551615}"#;
    assert_differential_parity(max_u64);

    let above_u64 = br#"{"n":18446744073709551616}"#;
    let simd = parse_simd_json(above_u64);
    let serde = parse_serde_json(above_u64);
    assert!(
        simd.is_err(),
        "simd-json current runtime path unexpectedly accepted u64+1 integer"
    );
    assert!(
        serde.is_ok(),
        "serde_json baseline unexpectedly rejected u64+1 integer"
    );

    let non_finite = br#"{"n":1e309}"#;
    assert_differential_parity(non_finite);
}
