//! A YAML message reaches a live consumer before the stream closes.

pub mod support;
use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use support::cli::cmd;

struct FollowProcess(Child);

impl Drop for FollowProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn yaml_follow_delivers_complete_documents_while_still_running() {
    let temp = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let output = cmd()
            .arg("--dir")
            .arg(temp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["pool", "create", "stream"]);
    run(&["feed", "stream", "{\"number\":1}"]);
    let child = cmd()
        .arg("--dir")
        .arg(temp.path())
        .args([
            "follow",
            "stream",
            "--yaml",
            "--tail",
            "1",
            "--timeout",
            "15s",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut process = FollowProcess(child);
    let stdout = process.0.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut document = String::new();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            document.push_str(&line);
            document.push('\n');
            if line == "..." && send.send(std::mem::take(&mut document)).is_err() {
                break;
            }
        }
    });
    for number in [1, 2] {
        if number == 2 {
            run(&["feed", "stream", "{\"number\":2}"]);
        }
        let document = receive
            .recv_timeout(Duration::from_secs(5))
            .expect("a complete YAML document must arrive without EOF or another message");
        let value: serde_json::Value = serde_yaml_ng::from_str(&document).unwrap();
        assert_eq!(value["data"]["number"], number);
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "follow should remain live"
        );
    }
    drop(process);
    reader.join().unwrap();
}
