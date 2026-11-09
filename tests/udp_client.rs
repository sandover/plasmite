use plasmite::api::{
    ErrorKind, PoolRef, UdpClient, UdpEvent, UdpFollowOptions, UdpSkipReason, UdpStart, lite3,
};
use serde_json::{Value, json};
use std::net::UdpSocket;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const HEADER: usize = 24;
const SUBSCRIBE: u8 = 1;
const CLOSE: u8 = 2;
const CHALLENGE: u8 = 3;
const PROGRESS: u8 = 4;
const SKIP: u8 = 5;
const DATA: u8 = 7;

fn packet(op: u8, id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut packet = b"\0PUDP\x01".to_vec();
    packet.extend_from_slice(&[op, 0]);
    packet.extend_from_slice(id);
    packet.extend_from_slice(body);
    packet
}

fn accept_subscription(peer: &UdpSocket) -> (Vec<u8>, std::net::SocketAddr, Value) {
    let mut received = [0u8; 4096];
    let (length, client) = peer.recv_from(&mut received).expect("initial subscribe");
    let request = &received[..length];
    assert_eq!(&request[..6], b"\0PUDP\x01");
    assert_eq!(&request[6..8], &[SUBSCRIBE, 0]);
    assert!(request.len() >= 256);
    let id = request[8..HEADER].to_vec();
    let subscribe: Value = serde_json::from_slice(&request[HEADER..]).expect("subscribe json");
    let cookie = json!({"expires_ms": 10_000, "mac": "mock-cookie"});
    let body = serde_json::to_vec(&json!({"cookie": cookie})).unwrap();
    peer.send_to(&packet(CHALLENGE, &id, &body), client)
        .expect("send challenge");

    let (length, source) = peer.recv_from(&mut received).expect("verified subscribe");
    let verified = &received[..length];
    assert_eq!(source, client);
    assert_eq!(verified[6], SUBSCRIBE);
    assert_eq!(&verified[8..HEADER], id);
    let verified: Value = serde_json::from_slice(&verified[HEADER..]).expect("verified json");
    assert_eq!(verified["cookie"], cookie);
    assert_eq!(verified["selection"], subscribe["selection"]);
    (id, client, subscribe)
}

#[test]
fn sends_complete_routed_lite3_datagrams_and_checks_both_sizes() {
    let peer = UdpSocket::bind("127.0.0.1:0").expect("bind peer");
    peer.set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout");
    let endpoint = peer.local_addr().expect("peer address");
    let client = UdpClient::new(format!("udp://{endpoint}")).expect("client");
    let pool = client.open_pool(&PoolRef::name("sensors")).expect("pool");
    let data = json!({"temperature": 23});
    let tags = vec!["lab".to_string()];
    let expected = lite3::encode_message(&tags, &data).expect("encode");

    pool.send_json(&data, &tags).expect("send json message");
    let mut received = [0u8; 65_507];
    let (length, _) = peer.recv_from(&mut received).expect("receive json message");
    let mut expected_packet = b"sensors\0".to_vec();
    expected_packet.extend_from_slice(expected.as_slice());
    assert_eq!(&received[..length], expected_packet);

    pool.send_lite3(expected.as_slice())
        .expect("send Lite3 message");
    let (length, _) = peer
        .recv_from(&mut received)
        .expect("receive Lite3 message");
    assert_eq!(&received[..length], expected_packet);

    let oversized = vec![0; 65_468];
    let error = pool.send_lite3(&oversized).expect_err("oversized payload");
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(error.message().unwrap_or_default().contains("65508"));

    let uri_pool = client
        .open_pool(&PoolRef::uri(format!("udp://{endpoint}/sensors")))
        .expect("same endpoint URL");
    assert!(uri_pool.send_lite3(&[0]).is_err());
}

#[test]
fn follows_network_frames_and_reports_initial_gap_after_data_arrives_first() {
    let peer = UdpSocket::bind("127.0.0.1:0").expect("bind peer");
    peer.set_read_timeout(Some(Duration::from_secs(3)))
        .expect("read timeout");
    let endpoint = peer.local_addr().expect("peer address");
    let (release_tx, release_rx) = mpsc::channel();
    let (progress_release_tx, progress_release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (id, client_address, subscribe) = accept_subscription(&peer);
        assert_eq!(subscribe["selection"]["pool"], "sensors");
        assert_eq!(subscribe["selection"]["start"]["kind"], "since");
        assert_eq!(
            subscribe["selection"]["start"]["value"],
            1_700_000_000_000_000_000u64
        );
        assert_eq!(subscribe["selection"]["live"], true);

        let payload = lite3::encode_message(&["lab".into()], &json!({"temperature": 23}))
            .expect("encode frame");
        let mut data = packet(DATA, &id, &[]);
        data.extend_from_slice(&3u64.to_be_bytes());
        data.extend_from_slice(&1_700_000_000_000_000_000u64.to_be_bytes());
        data.extend_from_slice(payload.as_slice());
        peer.send_to(&data, client_address)
            .expect("send data first");
        release_rx.recv().expect("client consumed data");

        let progress = json!({
            "start_seq": 1,
            "next_seq": 1,
            "end_seq": null,
            "lease_ms": 30_000,
            "terminal": false,
            "retention_gap": false
        });
        let progress_packet = packet(
            PROGRESS,
            &id,
            &serde_json::to_vec(&progress).expect("progress json"),
        );
        peer.send_to(&progress_packet, client_address)
            .expect("send progress later");

        progress_release_rx
            .recv()
            .expect("client consumed initial gap");
        let mut late_timestamp_data = packet(DATA, &id, &[]);
        late_timestamp_data.extend_from_slice(&4u64.to_be_bytes());
        late_timestamp_data.extend_from_slice(&1_699_999_999_999_999_999u64.to_be_bytes());
        late_timestamp_data.extend_from_slice(payload.as_slice());
        peer.send_to(&late_timestamp_data, client_address)
            .expect("send timestamp-filtered data");
        let progress = json!({
            "start_seq": 1,
            "next_seq": 5,
            "end_seq": null,
            "lease_ms": 30_000,
            "terminal": false,
            "retention_gap": false
        });
        let progress_packet = packet(
            PROGRESS,
            &id,
            &serde_json::to_vec(&progress).expect("progress json"),
        );
        peer.send_to(&progress_packet, client_address)
            .expect("send renewed progress");
    });

    let client = UdpClient::new(format!("udp://{endpoint}")).expect("client");
    let pool = client.open_pool(&PoolRef::name("sensors")).expect("pool");
    let mut follow = pool
        .follow(UdpFollowOptions {
            start: UdpStart::Since(1_700_000_000_000_000_000),
            live: true,
        })
        .expect("open follow");
    let frame = follow
        .next_event_timeout(Duration::from_secs(1))
        .expect("receive frame")
        .expect("frame event");
    let UdpEvent::Frame(frame) = frame else {
        panic!("expected frame first, got {frame:?}");
    };
    assert_eq!(frame.seq, 3);
    let message = frame.message().expect("decode shared message");
    assert_eq!(message.seq, 3);
    assert_eq!(message.meta.tags, ["lab"]);
    assert_eq!(message.data, json!({"temperature": 23}));
    release_tx.send(()).expect("release peer");

    let gap = follow
        .next_event_timeout(Duration::from_secs(1))
        .expect("receive initial gap")
        .expect("gap event");
    assert_eq!(
        gap,
        UdpEvent::Gap {
            first: 1,
            last: 2,
            reason: plasmite::api::UdpSkipReason::Unknown,
        }
    );
    assert!(
        follow
            .next_event_timeout(Duration::from_millis(20))
            .unwrap()
            .is_none()
    );
    assert!(
        !follow.is_finished(),
        "a quiet interval must leave follow open"
    );
    progress_release_tx.send(()).expect("release peer progress");
    let progress = follow
        .next_event_timeout(Duration::from_secs(1))
        .expect("receive progress after timestamp filter")
        .expect("progress event");
    assert!(matches!(
        progress,
        UdpEvent::Progress {
            next_seq: 5,
            retention_gap: false,
            ..
        }
    ));
    follow.close();
    server.join().expect("mock peer");
}

#[test]
fn finite_follow_drains_early_skips_and_terminal_progress_after_close() {
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let endpoint = peer.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (id, client, _) = accept_subscription(&peer);
        // Opening progress and the final DATA packet were lost. Two earlier SKIPs
        // arrive while the client is still opening.
        for (seq, reason) in [(2, "oversized"), (3, "pressure")] {
            let body =
                serde_json::to_vec(&json!({"first": seq, "last": seq, "reason": reason})).unwrap();
            peer.send_to(&packet(SKIP, &id, &body), client).unwrap();
        }
        let body = serde_json::to_vec(&json!({
            "start_seq": 2, "next_seq": 5, "end_seq": 4, "lease_ms": 30_000, "terminal": true
        }))
        .unwrap();
        peer.send_to(&packet(PROGRESS, &id, &body), client).unwrap();

        let mut close = [0; HEADER + 1];
        let (len, source) = peer.recv_from(&mut close).expect("close datagram");
        assert_eq!(source, client);
        assert_eq!(&close[..len], packet(CLOSE, &id, &[]));
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let err = peer
            .recv_from(&mut close)
            .expect_err("close must only send once");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
    });

    let pool = UdpClient::new(format!("udp://{endpoint}"))
        .unwrap()
        .open_pool(&PoolRef::name("sensors"))
        .unwrap();
    let mut follow = pool
        .follow(UdpFollowOptions {
            start: UdpStart::Last(3),
            live: false,
        })
        .unwrap();
    assert!(follow.is_finished(), "SKIP alone must not finish opening");
    follow.close();
    follow.close();
    assert_eq!(
        follow.next_event().unwrap(),
        Some(UdpEvent::Gap {
            first: 2,
            last: 4,
            reason: UdpSkipReason::Unknown
        })
    );
    assert_eq!(
        follow.next_event().unwrap(),
        Some(UdpEvent::Progress {
            start_seq: 2,
            next_seq: 5,
            end_seq: Some(4),
            terminal: true,
            retention_gap: false
        })
    );
    assert_eq!(follow.next_event().unwrap(), None);
    drop(follow);
    server.join().unwrap();
}

#[test]
fn follow_rejects_progress_that_changes_the_opening_range() {
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let endpoint = peer.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (id, client, _) = accept_subscription(&peer);
        for (start, next, end, terminal) in [
            (1, 1, 2, false),
            (1, 4, 3, true), // Different upper bound.
            (2, 3, 2, true), // Different start.
            (1, 3, 2, true), // Original range, final DATA lost.
        ] {
            let body = serde_json::to_vec(&json!({
                "start_seq": start, "next_seq": next, "end_seq": end,
                "lease_ms": 30_000, "terminal": terminal
            }))
            .unwrap();
            peer.send_to(&packet(PROGRESS, &id, &body), client).unwrap();
        }
    });

    let pool = UdpClient::new(format!("udp://{endpoint}"))
        .unwrap()
        .open_pool(&PoolRef::name("sensors"))
        .unwrap();
    let mut follow = pool
        .follow(UdpFollowOptions {
            start: UdpStart::Last(2),
            live: false,
        })
        .unwrap();
    let mut next = || follow.next_event_timeout(Duration::from_secs(1));
    assert!(matches!(
        next().unwrap(),
        Some(UdpEvent::Progress {
            terminal: false,
            ..
        })
    ));
    for _ in 0..2 {
        assert_eq!(
            next().expect_err("changed bounds").kind(),
            ErrorKind::Corrupt
        );
    }
    assert_eq!(
        next().unwrap(),
        Some(UdpEvent::Gap {
            first: 1,
            last: 2,
            reason: UdpSkipReason::Unknown
        })
    );
    assert!(matches!(
        next().unwrap(),
        Some(UdpEvent::Progress {
            start_seq: 1,
            end_seq: Some(2),
            terminal: true,
            ..
        })
    ));
    assert_eq!(next().unwrap(), None);
    server.join().unwrap();
}

#[test]
fn rejects_pool_paths_and_other_server_urls() {
    let peer = UdpSocket::bind("127.0.0.1:0").expect("bind peer");
    let endpoint = peer.local_addr().expect("peer address");
    let client = UdpClient::new(format!("udp://{endpoint}")).expect("client");

    for pool_ref in [
        PoolRef::path("sensors.plasmite"),
        PoolRef::uri(format!("udp://{endpoint}/pool/sensors")),
        PoolRef::uri(format!("udp://{endpoint}/bad%2Fname")),
        PoolRef::uri(format!("udp://{endpoint}/bad%5Cname")),
        PoolRef::uri("udp://127.0.0.1:1/sensors"),
    ] {
        assert_eq!(
            client
                .open_pool(&pool_ref)
                .expect_err("invalid pool ref")
                .kind(),
            ErrorKind::Usage
        );
    }
    assert!(UdpClient::new("udp://127.0.0.1/sensors").is_err());
}
