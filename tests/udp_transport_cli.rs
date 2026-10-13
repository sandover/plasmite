//! Actual CLI processes send and follow through the UDP endpoint.

use plasmite::api::Pool;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
        .args([
            "feed",
            &url,
            "--data",
            r#"{"temperature":23}"#,
            "--format",
            "json",
        ])
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
        .args([
            "follow",
            &url,
            "--tail",
            "1",
            "--no-follow",
            "--timeout",
            "6s",
            "--format",
            "json",
        ])
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
            "--data",
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
            "follow",
            &url,
            "--tail",
            "2",
            "--no-follow",
            "--tag",
            "alert",
            "--where",
            ".data.temperature == 24",
            "--timeout",
            "6s",
            "--format",
            "json",
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

    let timed_out = command()
        .args([
            "follow",
            &url,
            "--tag",
            "missing",
            "--timeout",
            "200ms",
            "--format",
            "json",
        ])
        .output()?;
    assert_eq!(
        timed_out.status.code(),
        Some(124),
        "{}",
        String::from_utf8_lossy(&timed_out.stderr)
    );
    assert!(timed_out.stdout.is_empty());

    let mut duplex = command()
        .args(["duplex", &url, "--format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    duplex
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"{\"data\":{\"from\":\"peer\",\"duplex\":true}}\n")?;
    let duplex_output = duplex.wait_with_output()?;
    assert!(
        duplex_output.status.success(),
        "{}",
        String::from_utf8_lossy(&duplex_output.stderr)
    );
    for line in duplex_output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let received: Value = serde_json::from_slice(line)?;
        assert!(
            received.get("seq").is_some(),
            "duplex output must be a received message"
        );
    }
    wait_for_seq(&pool_path, 3)?;
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
        vec!["fetch", "udp://127.0.0.1:9701/sensors", "1"],
        vec!["pool", "info", "udp://127.0.0.1:9701/sensors"],
        vec!["tap", "udp://127.0.0.1:9701/sensors", "--", "echo", "never"],
        vec!["access", "connect", "udp://127.0.0.1:9701/sensors"],
        vec![
            "feed",
            "udp://127.0.0.1:9701/sensors",
            "--create",
            "--data",
            "{}",
        ],
        vec![
            "follow",
            "udp://127.0.0.1:9701/sensors",
            "--tail",
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
