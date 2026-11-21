//! Actual CLI processes send and follow through the UDP endpoint.

use plasmite::api::Pool;
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub mod support;
use support::cli::{KillOnDrop, read_message_lines};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn udp_cli_feed_and_finite_follow_keep_send_receipt_free() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let pools = temp.path().join("pools");
    let ready = temp.path().join("ready");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&pools)?;
    let bin = env!("CARGO_BIN_EXE_plasmite");
    let command = || {
        let mut cmd = Command::new(bin);
        cmd.env("HOME", &home)
            .env("PLASMITE_ACCESS_HOME", &home)
            .arg("--dir")
            .arg(&pools);
        cmd
    };
    let created = command()
        .args(["pool", "create", "sensors", "--format", "json"])
        .output()?;
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let socket = UdpSocket::bind("127.0.0.1:0")?;
    let port = socket.local_addr()?.port();
    drop(socket);
    let child = command()
        .args([
            "serve",
            "--bind",
            "127.0.0.1:0",
            "--remote-bind",
            "127.0.0.1:0",
            &format!("--udp=127.0.0.1:{port}"),
            "--udp-follow",
        ])
        .env("PLASMITE_SERVE_READY_FILE", &ready)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut server = Server(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "server did not become ready");
        assert!(
            server.0.try_wait()?.is_none(),
            "server exited before readiness"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let url = format!("udp://127.0.0.1:{port}/sensors");
    let sent = command()
        .args(["feed", &url, r#"{"temperature":23}"#, "--format", "json"])
        .output()?;
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    assert!(sent.stdout.is_empty(), "UDP feed must not print a receipt");
    let notice: Value = serde_json::from_slice(&sent.stderr)?;
    assert_eq!(notice["notice"]["kind"], "udp-send");
    let pool_path = PathBuf::from(&pools).join("sensors.plasmite");
    wait_for_seq(&pool_path, 1)?;
    let followed = command()
        .args(["fetch", &url, "--last", "1", "--format", "json"])
        .output()?;
    assert!(
        followed.status.success(),
        "{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    let message: Value = serde_json::from_slice(&followed.stdout)?;
    assert_eq!(message["data"]["temperature"], 23);

    let tagged = command()
        .args([
            "feed",
            &url,
            "--tag",
            "alert",
            r#"{"temperature":24}"#,
            "--format",
            "json",
        ])
        .output()?;
    assert!(
        tagged.status.success(),
        "{}",
        String::from_utf8_lossy(&tagged.stderr)
    );
    wait_for_seq(&pool_path, 2)?;
    let filtered = command()
        .args([
            "fetch", &url, "--last", "2", "--tag", "alert", "--format", "json",
        ])
        .output()?;
    assert!(
        filtered.status.success(),
        "{}",
        String::from_utf8_lossy(&filtered.stderr)
    );
    let lines = filtered
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "only the tagged message should match");
    let matched: Value = serde_json::from_slice(lines[0])?;
    assert_eq!(matched["data"]["temperature"], 24);

    // Re-send a complete UDP follow output, preserving tags and replacing identity.
    let source: Value = serde_json::from_slice(lines[0])?;
    let copied = command()
        .args(["feed", &url, &source.to_string(), "--format", "json"])
        .output()?;
    assert!(
        copied.status.success(),
        "{}",
        String::from_utf8_lossy(&copied.stderr)
    );
    assert!(copied.stdout.is_empty());
    wait_for_seq(&pool_path, 3)?;
    let frame = Pool::open(&pool_path)?.get(3)?;
    let stored = plasmite::api::MessageEnvelope::from_lite3(frame.seq, frame.time, &frame.payload)?;
    assert_eq!(stored.data, source["data"]);
    assert_eq!(stored.meta.tags, vec!["alert"]);

    let filtered_empty = command()
        .args(["fetch", &url, "--tag", "missing", "--format", "json"])
        .output()?;
    assert!(filtered_empty.status.success());
    assert!(filtered_empty.stdout.is_empty());

    let mut duplex = KillOnDrop(
        command()
            .args(["duplex", &url, "--format", "json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    duplex
        .0
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"{\"from\":\"peer\",\"duplex\":true}\n")?;
    let bytes = read_message_lines(duplex.0.stdout.take().unwrap(), 1, Duration::from_secs(5));
    let message: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(message["data"], json!({"from":"peer","duplex":true}));
    wait_for_seq(&pool_path, 4)?;
    let all = command()
        .args(["fetch", &url, "--format", "json"])
        .output()?;
    assert!(
        all.status.success(),
        "{}",
        String::from_utf8_lossy(&all.stderr)
    );
    let messages = all
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(messages.len(), 4);
    assert_eq!(messages.first().unwrap()["seq"], 1);
    assert_eq!(messages.last().unwrap()["seq"], 4);
    Ok(())
}

fn wait_for_seq(path: &PathBuf, seq: u64) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if Pool::open(path)?.get(seq).is_ok() {
            return Ok(());
        }
        assert!(
            Instant::now() < deadline,
            "UDP message {seq} was not stored"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn unsupported_udp_commands_reject_without_server_work() -> Result<(), Box<dyn std::error::Error>> {
    let bin = env!("CARGO_BIN_EXE_plasmite");
    for args in [
        vec!["pool", "info", "udp://127.0.0.1:9701/sensors"],
        vec!["tap", "udp://127.0.0.1:9701/sensors", "--", "echo", "never"],
        vec!["access", "connect", "udp://127.0.0.1:9701/sensors"],
        vec!["feed", "udp://127.0.0.1:9701/sensors", "--create", "{}"],
        vec![
            "follow",
            "udp://127.0.0.1:9701/sensors",
            "--last",
            "1",
            "--replay",
            "1",
        ],
    ] {
        let output = Command::new(bin).args(args).output()?;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[test]
fn udp_cli_preserves_arrival_order_and_reports_only_server_skips()
-> Result<(), Box<dyn std::error::Error>> {
    for (skip, retained, filtered, code) in [
        (None, false, false, 0),
        (Some("oversized"), false, false, 1),
        (Some("pressure"), false, false, 1),
        (Some("retention"), true, false, 9),
        (None, true, false, 9), // The structured error works without a SKIP.
        (None, false, true, 0),
    ] {
        let peer = UdpSocket::bind("127.0.0.1:0")?;
        peer.set_read_timeout(Some(Duration::from_secs(3)))?;
        let address = peer.local_addr()?;
        let server = thread::spawn(move || {
            let mut buffer = [0; 4096];
            let (len, client) = peer.recv_from(&mut buffer).unwrap();
            assert_eq!(&buffer[..8], b"\0PUDP\x01\x01\0");
            let id = buffer[8..24].to_vec();
            let subscribe: Value = serde_json::from_slice(&buffer[24..len]).unwrap();
            assert_eq!(subscribe["selection"]["live"], false);
            let packet = |op, body: &[u8]| {
                let mut bytes = b"\0PUDP\x01".to_vec();
                bytes.extend_from_slice(&[op, 0]);
                bytes.extend_from_slice(&id);
                bytes.extend_from_slice(body);
                bytes
            };
            let cookie = serde_json::json!({"expires_ms": 10_000, "mac": "test-cookie"});
            peer.send_to(
                &packet(
                    3,
                    &serde_json::to_vec(&serde_json::json!({"cookie": cookie})).unwrap(),
                ),
                client,
            )
            .unwrap();
            let (len, source) = peer.recv_from(&mut buffer).unwrap();
            assert_eq!(source, client);
            let verified: Value = serde_json::from_slice(&buffer[24..len]).unwrap();
            assert_eq!(verified["cookie"], cookie);
            let data = |seq: u64| {
                let payload = plasmite::api::lite3::encode_message(
                    &[if seq.is_multiple_of(2) {
                        "even".into()
                    } else {
                        "odd".into()
                    }],
                    &serde_json::json!({"sample": seq}),
                )
                .unwrap();
                let mut bytes = seq.to_be_bytes().to_vec();
                bytes.extend_from_slice(&1_700_000_000_000_000_000u64.to_be_bytes());
                bytes.extend_from_slice(payload.as_slice());
                packet(7, &bytes)
            };
            peer.send_to(&data(10), client).unwrap(); // Open with DATA first.
            let progress = |terminal| {
                packet(
                    4,
                    &serde_json::to_vec(&serde_json::json!({
                        "start_seq": 10, "next_seq": 16, "end_seq": 15,
                        "lease_ms": 30_000, "terminal": terminal,
                    }))
                    .unwrap(),
                )
            };
            peer.send_to(&progress(false), client).unwrap(); // Progress overtakes DATA.
            if let Some(reason) = skip {
                peer.send_to(
                    &packet(
                        5,
                        &serde_json::to_vec(&serde_json::json!({
                            "first": 11, "last": 11, "reason": reason,
                        }))
                        .unwrap(),
                    ),
                    client,
                )
                .unwrap();
            }
            for seq in [12, 11, 12] {
                peer.send_to(&data(seq), client).unwrap();
            }
            // DATA 13 through 15 never arrive. Their absence creates no notice.
            if retained {
                peer.send_to(
                    &packet(
                        6,
                        &serde_json::to_vec(&serde_json::json!({
                            "kind": "RetentionGap", "message": "requested history rolled off",
                            "missing": 11, "next_seq": 12,
                        }))
                        .unwrap(),
                    ),
                    client,
                )
                .unwrap();
            } else {
                peer.send_to(&progress(true), client).unwrap();
            }
        });
        let temp = tempfile::tempdir()?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_plasmite"));
        command
            .env("HOME", temp.path())
            .env("PLASMITE_ACCESS_HOME", temp.path())
            .args([
                "fetch",
                &format!("udp://{address}/sensors"),
                "--last",
                "6",
                "--format",
                "json",
            ]);
        if filtered {
            command.args(["--tag", "even"]);
        }
        let output = command.output()?;
        server.join().unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let messages = output
            .stdout
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        let sequences: Vec<_> = messages
            .iter()
            .map(|m| m["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(
            sequences,
            if filtered {
                vec![10, 12, 12]
            } else if skip == Some("retention") {
                vec![10]
            } else {
                vec![10, 12, 11, 12]
            }
        );
        for message in &messages {
            assert_eq!(message["data"]["sample"], message["seq"]);
        }
        if retained {
            let error: Value = serde_json::from_slice(&output.stderr)?;
            assert_eq!(error["error"]["kind"], "RetentionGap");
            assert_eq!(error["error"]["missing"], 11);
            assert_eq!(error["error"]["next_seq"], 12);
        } else if let Some(reason) = skip {
            let notice: Value = serde_json::from_slice(&output.stderr)?;
            assert_eq!(notice["notice"]["kind"], "udp-skip");
            assert_eq!(notice["notice"]["details"]["first_seq"], 11);
            assert_eq!(notice["notice"]["details"]["last_seq"], 11);
            assert_eq!(notice["notice"]["details"]["reason"], reason);
        } else {
            assert!(output.stderr.is_empty());
        }
    }
    Ok(())
}

#[test]
fn udp_cli_resumes_a_reported_gap_and_requires_an_exact_fetch_sequence()
-> Result<(), Box<dyn std::error::Error>> {
    for live in [false, true] {
        let peer = UdpSocket::bind("127.0.0.1:0")?;
        peer.set_read_timeout(Some(Duration::from_secs(3)))?;
        let address = peer.local_addr()?;
        let server = thread::spawn(move || {
            let mut buffer = [0; 4096];
            let mut subscribe = || loop {
                let (len, client) = peer.recv_from(&mut buffer).unwrap();
                if buffer[6] == 1 {
                    let id: [u8; 16] = buffer[8..24].try_into().unwrap();
                    let request: Value = serde_json::from_slice(&buffer[24..len]).unwrap();
                    assert_eq!(request["selection"]["live"], live);
                    return (client, id, request["selection"]["start"].clone());
                }
            };
            let send = |client, id: [u8; 16], op, body: &[u8]| {
                let mut packet = b"\0PUDP\x01".to_vec();
                packet.extend_from_slice(&[op, 0]);
                packet.extend_from_slice(&id);
                packet.extend_from_slice(body);
                peer.send_to(&packet, client).unwrap();
            };
            let (mut client, mut id, start) = subscribe();
            assert_eq!(start, json!({"kind": "sequence", "value": 7}));
            if live {
                send(
                    client,
                    id,
                    4,
                    br#"{"start_seq":7,"next_seq":7,"lease_ms":30000,"terminal":false}"#,
                );
                send(client, id, 6, br#"{"kind":"RetentionGap","message":"history rolled off","missing":7,"next_seq":8}"#);
                let (resumed_client, resumed_id, resumed_start) = subscribe();
                assert_ne!(resumed_id, id);
                assert_eq!(resumed_start, json!({"kind": "sequence", "value": 8}));
                (client, id) = (resumed_client, resumed_id);
            }
            let payload = plasmite::api::lite3::encode_message(&[], &json!({"sample": 8})).unwrap();
            let mut data = 8u64.to_be_bytes().to_vec();
            data.extend_from_slice(&1_700_000_000_000_000_000u64.to_be_bytes());
            data.extend_from_slice(payload.as_slice());
            send(client, id, 7, &data);
            if !live {
                send(
                    client,
                    id,
                    4,
                    br#"{"start_seq":7,"next_seq":9,"end_seq":8,"lease_ms":30000,"terminal":true}"#,
                );
            }
        });
        let temp = tempfile::tempdir()?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_plasmite"));
        command
            .env("HOME", temp.path())
            .env("PLASMITE_ACCESS_HOME", temp.path());
        let pool = format!("udp://{address}/sensors");
        if live {
            command.args(["follow", &pool, "--since", "7", "--one"]);
        } else {
            command.args(["fetch", &pool, "7"]);
        }
        let output = command.args(["--format", "json"]).output()?;
        server.join().unwrap();
        let notice: Value = serde_json::from_slice(&output.stderr)?;
        if live {
            assert!(output.status.success(), "{notice}");
            let message: Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(message["seq"], 8);
            assert_eq!(notice["error"]["kind"], "RetentionGap");
            assert_eq!(notice["error"]["missing"], 7);
            assert_eq!(notice["error"]["next_seq"], 8);
        } else {
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert_eq!(notice["error"]["kind"], "NotFound");
            assert_eq!(notice["error"]["seq"], 7);
        }
    }
    Ok(())
}
