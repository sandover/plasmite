//! Run one lane per process. Setup/encoding stays outside the timed consumer.
//! Usage: bench_view_streams local|udp raw|raw_forward|view_selected|view_full|view_forward|json FIXTURE COUNT
use plasmite::api::{
    AppendOptions, Durability, Pool, PoolApiExt, PoolOptions, PoolRef, TailOptions, UdpClient,
    UdpEvent, UdpFollowOptions, UdpStart, UdpViewEvent,
};
use plasmite::view::MessageView;
use serde::Deserialize;
use serde_json::json;
use std::hint::black_box;
use std::net::UdpSocket;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(serde::Serialize, Deserialize)]
struct Input {
    value: i64,
    extra: serde_json::Value,
}
fn consume(view: &MessageView, lane: &str, destination: &mut Pool) -> i64 {
    if lane == "view_forward" {
        destination
            .append_view(view, AppendOptions::new(456, Durability::Fast))
            .unwrap();
        return 7;
    }
    if lane == "view_full" {
        view.data().decode::<Input>().unwrap().value
    } else {
        view.data()
            .get("value")
            .unwrap()
            .unwrap()
            .integer()
            .unwrap()
    }
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let transport = args.get(1).map(String::as_str).unwrap_or("local");
    let lane = args.get(2).map(String::as_str).unwrap_or("view_selected");
    let fixture = args.get(3).map(String::as_str).unwrap_or("small");
    let count: usize = args.get(4).map(|x| x.parse().unwrap()).unwrap_or(1000);
    assert!(count > 0);
    assert!(
        [
            "raw",
            "raw_forward",
            "view_selected",
            "view_full",
            "view_forward",
            "json"
        ]
        .contains(&lane)
    );
    let extra = match fixture {
        "small" => json!([true, "sensor"]),
        "wide" => (0..200)
            .map(|i| (format!("field_{i}"), json!(i)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
        "long" => json!("x".repeat(32000)),
        "array" => json!((0..1000).collect::<Vec<_>>()),
        _ => panic!("unknown fixture"),
    };
    let source = MessageView::encode(&Input { value: 7, extra }, &["sensor"]).unwrap();
    let (elapsed, sum) = match transport {
        "local" => local(lane, count, &source),
        "udp" => udp(lane, count, &source),
        _ => panic!("unknown transport"),
    };
    assert_eq!(sum, 7 * count as i64);
    println!(
        "{}",
        json!({"transport":transport,"lane":lane,"fixture":fixture,"count":count,"bytes":source.bytes().len(),"ns_per_message":elapsed.as_nanos() as f64/count as f64,"checksum":sum,"udp_policy":"one DATA in flight; exact delivery required"})
    );
}
fn local(lane: &str, count: usize, source: &MessageView) -> (Duration, i64) {
    let dir = tempfile::tempdir().unwrap();
    let capacity = (source.bytes().len() + 128)
        .checked_mul(count)
        .unwrap()
        .checked_mul(2)
        .unwrap()
        .checked_add(65536)
        .unwrap();
    let mut pool = Pool::create(
        dir.path().join("bench.plasmite"),
        PoolOptions::new(capacity as u64),
    )
    .unwrap();
    for _ in 0..count {
        pool.append_view(source, AppendOptions::new(123, Durability::Fast))
            .unwrap();
    }
    let options = TailOptions {
        since_seq: Some(1),
        max_messages: Some(count),
        notify: false,
        ..TailOptions::new()
    };
    let mut destination = Pool::create(
        dir.path().join("forward.plasmite"),
        PoolOptions::new(capacity as u64),
    )
    .unwrap();
    let start = Instant::now();
    let mut sum = 0;
    match lane {
        "raw" | "raw_forward" => {
            let mut tail = pool.tail_lite3(options);
            while let Some(frame) = tail.next_frame().unwrap() {
                if lane == "raw_forward" {
                    destination
                        .append_lite3(&frame.payload, AppendOptions::new(456, Durability::Fast))
                        .unwrap();
                }
                black_box(frame.payload);
                sum += 7;
            }
        }
        "json" => {
            let mut tail = pool.tail(options);
            while let Some(message) = tail.next_message().unwrap() {
                sum += message.data["value"].as_i64().unwrap();
                black_box(message);
            }
        }
        _ => {
            for view in pool.tail_views(options) {
                let view = view.unwrap();
                sum += consume(&view, lane, &mut destination);
                black_box(view);
            }
        }
    }
    let elapsed = start.elapsed();
    if lane.ends_with("forward") {
        assert_eq!(
            destination.get_lite3(count as u64).unwrap().payload,
            source.bytes()
        );
    }
    (elapsed, sum)
}
fn packet(op: u8, id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut p = b"\0PUDP\x01".to_vec();
    p.extend_from_slice(&[op, 0]);
    p.extend_from_slice(id);
    p.extend_from_slice(body);
    p
}
fn udp(lane: &str, count: usize, source: &MessageView) -> (Duration, i64) {
    let dir = tempfile::tempdir().unwrap();
    let mut destination = Pool::create(
        dir.path().join("forward.plasmite"),
        PoolOptions::new(((source.bytes().len() + 128) * count * 2 + 65536) as u64),
    )
    .unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket2::SockRef::from(&peer)
        .set_send_buffer_size(128 * 1024)
        .unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let endpoint = peer.local_addr().unwrap();
    let bytes = source.bytes().to_vec();
    let (ack_tx, ack_rx) = mpsc::sync_channel(0);
    let sender = std::thread::spawn(move || {
        let mut request = [0u8; 4096];
        let (n, client) = peer.recv_from(&mut request).unwrap();
        assert!(n >= 24);
        let id = request[8..24].to_vec();
        // A direct PROGRESS response is valid for an already reachable test peer.
        let progress = serde_json::to_vec(
            &json!({"start_seq":1,"next_seq":1,"end_seq":null,"lease_ms":30000,"terminal":false}),
        )
        .unwrap();
        peer.send_to(&packet(4, &id, &progress), client).unwrap();
        ack_rx.recv().unwrap();
        let mut body = vec![0u8; 16];
        body.extend_from_slice(&bytes);
        for seq in 1..=count as u64 {
            body[..8].copy_from_slice(&seq.to_be_bytes());
            peer.send_to(&packet(7, &id, &body), client).unwrap();
            ack_rx.recv().unwrap();
        }
    });
    let pool = UdpClient::new(format!("udp://{endpoint}"))
        .unwrap()
        .open_pool(&PoolRef::name("bench"))
        .unwrap();
    let options = UdpFollowOptions {
        start: UdpStart::Sequence(1),
        live: true,
    };
    let mut sum = 0;
    let elapsed = if lane.starts_with("view_") {
        let mut follow = pool.follow_views(options).unwrap();
        assert!(matches!(
            follow.next_event().unwrap(),
            Some(UdpViewEvent::Progress { .. })
        ));
        let start = Instant::now();
        ack_tx.send(()).unwrap();
        for seq in 1..=count as u64 {
            let Some(UdpViewEvent::Frame(view)) =
                follow.next_event_timeout(Duration::from_secs(5)).unwrap()
            else {
                panic!("missing DATA")
            };
            assert_eq!(view.seq, seq);
            sum += consume(&view, lane, &mut destination);
            black_box(view);
            ack_tx.send(()).unwrap();
        }
        start.elapsed()
    } else {
        let mut follow = pool.follow(options).unwrap();
        assert!(matches!(
            follow.next_event().unwrap(),
            Some(UdpEvent::Progress { .. })
        ));
        let start = Instant::now();
        ack_tx.send(()).unwrap();
        for seq in 1..=count as u64 {
            let Some(UdpEvent::Frame(frame)) =
                follow.next_event_timeout(Duration::from_secs(5)).unwrap()
            else {
                panic!("missing DATA")
            };
            assert_eq!(frame.seq, seq);
            if lane == "json" {
                sum += frame.message().unwrap().data["value"].as_i64().unwrap();
            } else {
                sum += 7;
                if lane == "raw_forward" {
                    destination
                        .append_lite3(&frame.payload, AppendOptions::new(456, Durability::Fast))
                        .unwrap();
                }
                black_box(frame.payload);
            }
            ack_tx.send(()).unwrap();
        }
        start.elapsed()
    };
    sender.join().unwrap();
    if lane.ends_with("forward") {
        assert_eq!(
            destination.get_lite3(count as u64).unwrap().payload,
            source.bytes()
        );
    }
    (elapsed, sum)
}
