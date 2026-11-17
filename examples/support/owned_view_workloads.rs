//! Focused immutable-view comparison; timings exclude fixture construction.
//! Run: cargo run --release --example bench_owned_views -- 2000
use plasmite::api::{Message, Pool, PoolOptions, lite3::Lite3Buf};
use plasmite::view::MessageView;
use std::hint::black_box;
use std::time::Instant;
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct Reading<'a> {
    power: i64,
    samples: Vec<i64>,
    note: Option<&'a str>,
}
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
        let native = Message::from_lite3(17, 1, bytes).unwrap();
        let expected_typed: Reading<'_> = serde::Deserialize::deserialize(&native.data).unwrap();
        assert_eq!(v.data().decode::<Reading<'_>>().unwrap(), expected_typed);
        let direct = MessageView::encode(&data, &[]).unwrap();
        let json = serde_json::to_vec(&data).unwrap();
        let via_json = MessageView::from_json(&json, &[]).unwrap();
        assert_eq!(direct.data().decode::<serde_json::Value>().unwrap(), data);
        assert_eq!(via_json.data().decode::<serde_json::Value>().unwrap(), data);
        let typed_direct = MessageView::encode(&expected_typed, &[]).unwrap();
        let typed_json = serde_json::to_vec(&expected_typed).unwrap();
        let typed_via_json = MessageView::from_json(&typed_json, &[]).unwrap();
        assert_eq!(
            typed_direct.data().to_json().unwrap(),
            typed_via_json.data().to_json().unwrap()
        );
        let directory = tempfile::tempdir().unwrap();
        let mut pool = Pool::create(
            directory.path().join("typed.plasmite"),
            PoolOptions::new(8 * 1024 * 1024),
        )
        .unwrap();
        let committed = pool
            .feed_view(
                &MessageView::encode(&expected_typed, &[]).unwrap(),
                plasmite::api::Feed::default(),
            )
            .unwrap();
        assert_eq!(
            committed.data().decode::<Reading<'_>>().unwrap(),
            expected_typed
        );
        assert_eq!(
            pool.fetch_view(plasmite::api::Read::since(plasmite::api::Since::Seq(
                committed.seq
            )))
            .unwrap()
            .next_view()
            .unwrap()
            .unwrap()
            .data()
            .decode::<Reading<'_>>()
            .unwrap(),
            expected_typed
        );
        let retagged = MessageView::encode(&v.data(), &["changed"]).unwrap();
        assert_eq!(retagged.data().decode::<serde_json::Value>().unwrap(), data);
        let mut lanes = [
            "append_typed_direct",
            "append_typed_json",
            "retag_view_direct",
            "retag_view_json",
            "encode_native_direct",
            "encode_native_json",
            "encode_typed_direct",
            "encode_typed_json",
            "owned_child_share",
            "warm_view_typed",
            "warm_json_typed",
            "warm_native_typed",
            "cold_view_typed",
            "cold_legacy_typed",
            "warm_view_full_native",
            "warm_json_full_native",
            "whole_root_validation",
            "snapshot_copy",
            "cold_view_selected",
            "cold_json_selected",
            "warm_view_selected",
            "warm_native_selected",
            "view_array_cursor",
            "native_array_iteration",
            "view_json",
            "view_array_enumeration",
        ];
        if reverse {
            lanes.reverse();
        }
        for lane in lanes {
            match lane {
                "owned_child_share" => run(name, lane, n, || {
                    black_box(black_box(samples).into_owned());
                }),
                "append_typed_direct" => run(name, lane, n, || {
                    let committed = pool
                        .feed_view(
                            &MessageView::encode(black_box(&expected_typed), &[]).unwrap(),
                            plasmite::api::Feed::default(),
                        )
                        .unwrap();
                    assert!(committed.seq > 0);
                    black_box(committed);
                }),
                "append_typed_json" => run(name, lane, n, || {
                    let json = serde_json::to_vec(black_box(&expected_typed)).unwrap();
                    let encoded = MessageView::from_json(&json, &[]).unwrap();
                    let committed = pool
                        .feed_view(&encoded, plasmite::api::Feed::default())
                        .unwrap();
                    assert!(committed.seq > 0);
                    black_box(committed);
                }),
                "retag_view_direct" => run(name, lane, n, || {
                    black_box(MessageView::encode(&black_box(v.data()), &["changed"]).unwrap());
                }),
                "retag_view_json" => run(name, lane, n, || {
                    let json = black_box(v.data()).to_json().unwrap();
                    black_box(MessageView::from_json(&json, &["changed"]).unwrap());
                }),
                "encode_native_direct" => run(name, lane, n, || {
                    black_box(MessageView::encode(black_box(&data), &[]).unwrap());
                }),
                "encode_native_json" => run(name, lane, n, || {
                    let json = serde_json::to_vec(black_box(&data)).unwrap();
                    black_box(MessageView::from_json(&json, &[]).unwrap());
                }),
                "encode_typed_direct" => run(name, lane, n, || {
                    black_box(MessageView::encode(black_box(&expected_typed), &[]).unwrap());
                }),
                "encode_typed_json" => run(name, lane, n, || {
                    let json = serde_json::to_vec(black_box(&expected_typed)).unwrap();
                    black_box(MessageView::from_json(&json, &[]).unwrap());
                }),
                "warm_view_typed" => run(name, lane, n, || {
                    let reading: Reading<'_> = black_box(v.data()).decode().unwrap();
                    assert_eq!(reading, expected_typed);
                    black_box(reading);
                }),
                "warm_json_typed" => run(name, lane, n, || {
                    let json = black_box(v.data()).to_json().unwrap();
                    let reading: Reading<'_> = serde_json::from_slice(&json).unwrap();
                    assert_eq!(reading, expected_typed);
                    black_box(reading);
                }),
                "warm_native_typed" => run(name, lane, n, || {
                    let reading: Reading<'_> =
                        serde::Deserialize::deserialize(black_box(&native.data)).unwrap();
                    assert_eq!(reading, expected_typed);
                    black_box(reading);
                }),
                "cold_view_typed" => run(name, lane, n, || {
                    let owner = MessageView::from_bytes(bytes, 17, 1).unwrap();
                    let reading: Reading<'_> = owner.data().decode().unwrap();
                    assert_eq!(reading, expected_typed);
                    black_box(reading);
                }),
                "cold_legacy_typed" => run(name, lane, n, || {
                    let snapshot = bytes.to_vec();
                    let message = Message::from_lite3(17, 1, &snapshot).unwrap();
                    let reading: Reading<'_> =
                        serde::Deserialize::deserialize(&message.data).unwrap();
                    assert_eq!(reading, expected_typed);
                    black_box(reading);
                }),
                "warm_view_full_native" => run(name, lane, n, || {
                    let decoded: serde_json::Value = black_box(v.data()).decode().unwrap();
                    assert_eq!(decoded, native.data);
                    black_box(decoded);
                }),
                "warm_json_full_native" => run(name, lane, n, || {
                    let json = black_box(v.data()).to_json().unwrap();
                    let decoded: serde_json::Value = serde_json::from_slice(&json).unwrap();
                    assert_eq!(decoded, native.data);
                    black_box(decoded);
                }),
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
                "warm_native_selected" => run(name, lane, n, || {
                    assert_eq!(black_box(&native.data)["power"].as_i64(), Some(7));
                }),
                "view_array_cursor" => run(name, lane, n, || {
                    let sum: i64 = samples
                        .iter()
                        .unwrap()
                        .map(|entry| entry.unwrap().1.integer().unwrap())
                        .sum();
                    assert_eq!(black_box(sum), expected_sum);
                }),
                "native_array_iteration" => run(name, lane, n, || {
                    let sum: i64 = native.data["samples"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_i64().unwrap())
                        .sum();
                    assert_eq!(black_box(sum), expected_sum);
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
