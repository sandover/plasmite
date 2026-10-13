use plasmite::api::{ErrorKind, PoolRef, UdpClient, UdpEvent, UdpFollowOptions, UdpStart, lite3};
use serde_json::{Value, json};
use std::net::UdpSocket;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const HEADER: usize = 24;
const SUBSCRIBE: u8 = 1;
const CHALLENGE: u8 = 3;
const PROGRESS: u8 = 4;
const DATA: u8 = 7;

fn packet(op: u8, id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut packet = b"\0PUDP\x01".to_vec();
    packet.extend_from_slice(&[op, 0]);
    packet.extend_from_slice(id);
    packet.extend_from_slice(body);
    packet
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
        let mut received = [0u8; 4096];
        let (length, client_address) = peer.recv_from(&mut received).expect("initial subscribe");
        let request = &received[..length];
        assert_eq!(&request[..6], b"\0PUDP\x01");
        assert_eq!(request[6], SUBSCRIBE);
        assert_eq!(request[7], 0);
        assert!(request.len() >= 256);
        let id = request[8..HEADER].to_vec();
        let subscribe: Value = serde_json::from_slice(&request[HEADER..]).expect("subscribe json");
        assert_eq!(subscribe["selection"]["pool"], "sensors");
        assert_eq!(subscribe["selection"]["start"]["kind"], "since");
        assert_eq!(
            subscribe["selection"]["start"]["value"],
            1_700_000_000_000_000_000u64
        );
        assert_eq!(subscribe["selection"]["live"], true);

        let challenge = json!({
            "cookie": {"expires_ms": 10_000, "mac": "mock-cookie"}
        });
        let challenge_packet = packet(
            CHALLENGE,
            &id,
            &serde_json::to_vec(&challenge).expect("challenge json"),
        );
        peer.send_to(&challenge_packet, client_address)
            .expect("send challenge");

        let (length, client_address) = peer.recv_from(&mut received).expect("verified subscribe");
        let verified = &received[..length];
        assert_eq!(verified[6], SUBSCRIBE);
        assert_eq!(&verified[8..HEADER], id);
        let verified: Value = serde_json::from_slice(&verified[HEADER..]).expect("verified json");
        assert_eq!(verified["cookie"]["mac"], "mock-cookie");
        assert_eq!(verified["selection"]["pool"], "sensors");

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
