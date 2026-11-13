//! Focused immutable-view comparison; timings exclude fixture construction.
//! Run: cargo run --release --example bench_owned_views -- 2000
use plasmite::api::{Message, lite3::Lite3Buf};
use plasmite::view::MessageView;
use std::hint::black_box;
use std::time::Instant;
fn run<F: FnMut()>(fixture: &str, lane: &str, n: usize, mut f: F) {
    for _ in 0..10 {
        f();
    }
    crate::reset_probe();
    let start = Instant::now();
    for _ in 0..n {
        f();
    }
    let nanos = start.elapsed().as_nanos();
    let counts = crate::probe_counts();
    println!(
        "{}",
        serde_json::json!({"fixture":fixture,"lane":lane,"iterations":n,"ns_per_op":if counts.is_none(){Some(nanos as f64/n as f64)}else{None},"allocation_probe":counts.map(|(allocs,bytes)|serde_json::json!({"rust_allocs_per_op":allocs as f64/n as f64,"rust_requested_bytes_per_op":bytes as f64/n as f64}))})
    );
}
pub fn main() {
    let mut args = std::env::args().skip(1);
    let n = args
        .next()
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(2000);
    assert!(n > 0);
    let reverse = args.any(|argument| argument == "--reverse");
    let small = serde_json::json!({"power":7,"samples":[1,2,3]});
    let mut wide = small.clone();
    for i in 0..200 {
        wide[format!("field{i}")] = serde_json::json!({"value":i,"name":"sensor"});
    }
    let mut large = small.clone();
    large["note"] = serde_json::json!("x".repeat(65536));
    let mut array = small.clone();
    array["samples"] = serde_json::json!((0..1000).collect::<Vec<_>>());
    println!(
        "{}",
        serde_json::json!({"reverse_lane_order":reverse,"note":"Rust allocator counts exclude C malloc and mapped storage. Cold selected lanes include one snapshot copy; warm lanes reuse one validated owner. Repeat in alternating order externally. Retained child refs hold the complete encoded payload."})
    );
    for (name, data) in [
        ("small", small),
        ("wide", wide),
        ("long_string", large),
        ("array", array),
    ] {
        let expected_sum: i64 = data["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_i64().unwrap())
            .sum();
        let encoded = Lite3Buf::from_json_str(
            &serde_json::json!({"meta":{"tags":[]},"data":data}).to_string(),
        )
        .unwrap();
        let bytes = encoded.as_slice();
        let v = MessageView::from_bytes(bytes, 17, 1).unwrap();
        let samples = v.data().get("samples").unwrap().unwrap();
        let mut lanes = [
            "whole_root_validation",
            "snapshot_copy",
            "cold_view_selected",
            "cold_json_selected",
            "warm_view_selected",
            "view_json",
            "view_array_enumeration",
        ];
        if reverse {
            lanes.reverse();
        }
        for lane in lanes {
            match lane {
                "whole_root_validation" => run(name, lane, n, || {
                    assert!(
                        unsafe {
                            plasmite::api::lite3::sys::plasmite_lite3_verify_json(
                                bytes.as_ptr(),
                                bytes.len(),
                                0,
                            )
                        } >= 0
                    );
                }),
                "snapshot_copy" => run(name, lane, n, || {
                    black_box(bytes.to_vec());
                }),
                "cold_view_selected" => run(name, lane, n, || {
                    let owner = MessageView::from_bytes(bytes, 17, 1).unwrap();
                    assert_eq!(
                        owner.data().get("power").unwrap().unwrap().integer(),
                        Some(7)
                    );
                    black_box(owner);
                }),
                "cold_json_selected" => run(name, lane, n, || {
                    let snapshot = bytes.to_vec();
                    let message = Message::from_lite3(17, 1, &snapshot).unwrap();
                    assert_eq!(message.data["power"], 7);
                    black_box(message);
                }),
                "warm_view_selected" => run(name, lane, n, || {
                    assert_eq!(
                        black_box(v.data()).get("power").unwrap().unwrap().integer(),
                        Some(7)
                    );
                }),
                "view_json" => run(name, lane, n, || {
                    black_box(v.data().to_json().unwrap());
                }),
                "view_array_enumeration" => run(name, lane, n, || {
                    let sum: i64 = samples
                        .entries()
                        .unwrap()
                        .iter()
                        .map(|(_, x)| x.integer().unwrap())
                        .sum();
                    assert_eq!(black_box(sum), expected_sum);
                }),
                _ => unreachable!(),
            }
        }
        println!(
            "{}",
            serde_json::json!({"fixture":name,"retained_encoded_payload_bytes_for_child":v.bytes().len()})
        );
    }
}
